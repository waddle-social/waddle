# P2.1 enabled caller-to-object map, source review

Status: source-reviewed inventory based on `origin/main` at
`1188ffb2b1655cfe09684ce0d86395daf8bb4535`, with this PR's additional
combined-fixture owner phase included. Independent review remains open. This is
an index into current source, not a complete catalog closure or serving profile.
It grants no startup or serving authority.

## Target and current reachability

The candidate is PostgreSQL 17, built with `clustering`, runtime clustering
disabled, HomeServer, XMPP and native auth enabled, SpiceDB configured, no
external auth provider, no configured extensions or modules, and no APNs/FCM
provider. The global URL is nonempty and all seven child locations inherit that
same URL. The target shape and its caveats are recorded in the
[known-coverage gate](./2116-known-coverage-gap-gate.md#L11-L28).

`inspect_coverage_gap_gate_v1` accepts a caller-described shape only; it has no
success result, does not open a database, and is compiled behind `test-support`
([implementation](../../server/crates/waddle-server/src/db/physical_attestation/coverage_gap_gate.rs#L1-L7),
[export](../../server/crates/waddle-server/src/db/physical_attestation.rs#L51-L57)).
The production attestation environment path currently stops in `main` with
`UnconvertedStores` after read-only preflight ([main.rs](../../server/crates/waddle-server/src/main.rs#L67-L95));
`start_with_config_and_owner_outbox` also rejects an attested pool before
binding listeners or starting actors ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L91-L104)).
Consequently, none of the application store startup calls below is currently
reachable from an attested serving process. “Enabled” below means selected by
the candidate runtime configuration if the startup gate is later converted;
it does not describe a running server.

The build/runtime distinction changes store selection: with runtime clustering
disabled, `start_if_enabled` returns empty handles before creating claim stores
or a durable MUC store ([node.rs](../../server/crates/waddle-server/src/clustering/node.rs#L921-L945),
[MUC initialization](../../server/crates/waddle-server/src/clustering/node.rs#L1053-L1077)).
`main` only creates the separate control-plane pool when clustering is enabled,
so the candidate has no selected `ControlPlane` database either
([main.rs](../../server/crates/waddle-server/src/main.rs#L97-L121)). External
auth providers and extension modules are absent by target configuration; APNs
and FCM provider branches are not selected, although Web Push and its database
stores are still started by the ordinary HTTP path ([provider branches](../../server/crates/waddle-server/src/server/http.rs#L1371-L1421)).
The compiled MAM selector nevertheless rejects an attested pool when clustering
is disabled or its claim pair is absent ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L532-L555));
the combined fixture asserts this exact refusal
([coverage-gap fixture](../../server/crates/waddle-server/tests/physical_attestation/coverage_gap_gate.rs#L340-L359)).

## Caller and object map

The object names below are the currently evidenced names, often representative
only. `2116-known-coverage-gap-gate.md` explicitly says these names are not a
complete per-family inventory and that the DurableStore registry cannot prove
the global-schema consumer set or catalog completeness
([lines 34–43](./2116-known-coverage-gap-gate.md#L34-L43)).

| Gate 1 gap | Candidate caller/reachability | Current object evidence and remaining mapping |
| --- | --- | --- |
| `JointRoleGraph` | Prospective common profile for all stores. Current attested startup is blocked as above. | No joined role graph. The combined fixture has separate database owner, schema owner, runtime, API, helper, migrator, installer, and definer roles; the gate target itself carries only database-owner and schema-owner OIDs. See [fixture roles](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs#L213-L255) and [gap declaration](../../server/crates/waddle-server/src/db/physical_attestation/coverage_gap_gate.rs#L109-L116). Distinct owner OIDs do not prove all role relationships or effective rights. |
| `GlobalMigrations` | Always part of owner bootstrap before normal server startup; the shared-owner bootstrap calls the registered migration runner ([main.rs](../../server/crates/waddle-server/src/main.rs#L97-L133)). | `_lineage` and `_migrations` have a narrow runtime read-only check. The global migration set is much larger; see [global DDL](../../server/crates/waddle-server/src/db/migrations/global.rs#L23-L167) and [waddle DDL](../../server/crates/waddle-server/src/db/migrations/waddle.rs#L1-L20). No exact inventory of every relation, sequence, routine, or dependency is attached to the check. |
| `IdentityAndSessions` | Native auth and application identity/session callers remain enabled by the target. | `users`, `auth_identities`, `sessions`, and `native_users`; migration DDL is in [global.rs](../../server/crates/waddle-server/src/db/migrations/global.rs#L23-L97), with live auth callers in [identity.rs](../../server/crates/waddle-server/src/auth/identity.rs) and [native.rs](../../server/crates/waddle-server/src/auth/native.rs). Their runtime role grants are not in the joint matrix. |
| `RosterBlockingProfileUploads` | Global XMPP and account surfaces remain enabled. | `roster_items`, `roster_versions`, `blocking_list`, `vcard_storage`, and `upload_slots` are migration objects ([global.rs](../../server/crates/waddle-server/src/db/migrations/global.rs#L99-L155)); blocking and roster consumers include [blocking.rs](../../server/crates/waddle-server/src/db/blocking.rs) and [roster.rs](../../server/crates/waddle-server/src/db/roster.rs). Column-level and dependent-object ACLs are not classified jointly. |
| `PermissionBackend` | SpiceDB is selected by the target: normal lifecycle constructs `PermissionActor::from_server_config` when this is not the fixed test backend ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L167-L182)). | The production tuple store is external SpiceDB. `permission_tuples` remains a migrated local object ([global.rs](../../server/crates/waddle-server/src/db/migrations/global.rs#L64-L79)); it is not proof of production permission storage or of its SQL ACL needs. |
| `Inbox` | Selected portable Inbox attachment uses the global handle when attested; target provenance must be inherited ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L347-L399)). | `inbox_entries`, `groupchat_notification_recovery`. Exact R table rights are already checked for these two relations in [runtime_store_acl.rs](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L16-L29), but not as part of a complete joint graph. |
| `StreamManagement` | With clustering disabled, the portable SM path is selected; attested attachment is `DatabaseSmPersistence::attach_runtime_postgres` ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1927-L1957), [selector](../../server/crates/waddle-server/src/server/http.rs#L2104-L2132)). | `sm_sessions`, `sm_unacked`, `sm_acknowledged_windows`, `sm_ingress_appends`. The local relation/DML matrix exists in [runtime_store_acl.rs](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L44-L69). Detached/raw writer conversion and whole-profile closure remain separate. |
| `PendingDelivery` | Selected, same-global-pool attach in the attested branch ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L2134-L2153)). | `pending_delivery`, `pending_delivery_sm_promotions`, `pending_delivery_startup_migrations`; the first three relation ACL shapes and sequence rule are partly checked by [runtime_store_acl.rs](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L70-L89) and [sequence verifier](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L266-L285). Trigger/function dependencies still need closure. |
| `IngressEpoch` | Ingress transaction authority is opened in HTTP startup ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L900-L925)); attested serving remains globally blocked. | The fixed partial profile names 13 ingress heaps plus epoch and guard manifest ([pinned_partial_profile.rs](../../server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs#L19-L33), [checks](../../server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs#L143-L212)). It does not inventory all functions, triggers, indexes, or dependencies. |
| `ReceiptJournal` | Receipt operations are part of ingress storage; no whole-profile startup attachment makes them selected under attested serving yet. | `p21_receipt_operation_results` and `waddle_p21_record_effect_receipt` are representative V1030 journal objects ([migration](../../server/crates/waddle-server/src/db/migrations/owner/p21_receipt_journal_postgres.sql), [partial receipt authority](../../server/crates/waddle-server/src/db/physical_attestation/pinned_receipt.rs)). Their role/function dependency closure is not in the joint matrix. |
| `PubSubProjections` | PubSub attaches the global handle for an attested target; the same handle backs the notification projections ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L208-L219), [selector](../../server/crates/waddle-server/src/server/lifecycle.rs#L550-L580), [http.rs](../../server/crates/waddle-server/src/server/http.rs#L1451-L1455)). | Existing partial profile has 12 named relations and per-table R/H rights ([pinned_partial_profile.rs](../../server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs#L35-L62), [verifier](../../server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs#L197-L212)). It is not complete PubSub helper/function/dependency closure. |
| `MamAndClaimPair` | MAM selection is called from lifecycle, but is unreachable for the candidate attested profile because the build has clustering and runtime clustering/claims are disabled ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L407-L420), [http.rs](../../server/crates/waddle-server/src/server/http.rs#L539-L555)). | `mam_messages`, `mam_archive_sequences`; the combined test prepares these, but no valid claim pair exists in this target. The test calls and asserts the refusal rather than qualifying MAM. |
| `SpacesMetadata` | Selected as an inherited-global store alongside links; the attested path calls `attach_existing` ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L583-L612)). | `spaces_metadata` is now owner-initialized in the additional fixture and gets a table ACL check. The paired `_waddle_space_store_owner` marker is intentionally left empty by those legacy initializers, so this does not certify the strict paired preparation or attach path ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L151-L159), [marker check](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L182-L188)). |
| `ChannelSpaceLinks` | Selected together with spaces metadata on the same inherited global handle ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L583-L612)). | `channel_space_links`; schema source is [channel_space_links/schema.rs](../../server/crates/waddle-server/src/channel_space_links/schema.rs#L1-L38). The extra fixture initializes it and checks its table ACL, but has the same unpaired owner marker limitation as spaces metadata ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L151-L159)). |
| `RoomCatalog` | Local MUC registry, channel admin, and invite routes remain configured. Relevant source includes [websocket MUC invites](../../server/crates/waddle-server/src/server/routes/websocket/muc_invites.rs) and [admin channels](../../server/crates/waddle-server/src/admin/channels.rs). | `channels`, `muc_pending_invites`, `muc_invite_claims`, `group_dm_archive_boundaries` are migration objects ([channels](../../server/crates/waddle-server/src/db/migrations/waddle.rs#L12-L20), [group-DM/invite DDL](../../server/crates/waddle-server/src/db/migrations/waddle.rs#L200-L235), [invite-claim DDL](../../server/crates/waddle-server/src/db/migrations/waddle.rs#L533-L543), [PostgreSQL invite-claim DDL](../../server/crates/waddle-server/src/db/migrations/waddle.rs#L708-L718)). `muc_invite_claims` also references ingress message state; its runtime ACL and FK/trigger closure are not in the selected 13-ingress partial profile. |
| `MucFixtureTarget` | Runtime clustered room store is disabled: `start_if_enabled` returns empty handles before its MUC-store initializer. | The combined fixture nevertheless initializes clustered MUC rooms/outboxes under `#[cfg(feature = "clustering")]`, independent of runtime enablement ([fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs#L789-L809)). This fixture does not match the candidate runtime-selected stores. |
| `MucDestroyOutbox` | A startup attach/owner-init call occurs before the clustering-enabled branch in lifecycle ([lifecycle.rs](../../server/crates/waddle-server/src/server/lifecycle.rs#L148-L166)); it is called even when runtime clustering is disabled if startup reaches this point. | `clustering_muc_destroy_outbox`. No joint profile check; fixture prepares it with feature-gated clustered MUC setup, despite the target not selecting durable clustered MUC. |
| `RoomEffectOutbox` | HTTP startup calls `RoomEffectOutboxStore::try_new` without an attested attach branch ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1078-L1085)). Under a future attested startup this currently conflicts; the combined fixture asserts `OwnerPreparationRequired` ([fixture](../../server/crates/waddle-server/tests/physical_attestation/coverage_gap_gate.rs#L360-L363)). | `clustering_muc_room_effects`. The fixture has a separately owner-prepared effect table, but it does not make startup select or attach it as a supported store. |
| `PushRegistrations` | XEP-0357 storage is initialized unconditionally in HTTP startup, irrespective of APNs/FCM config ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1317-L1322)). Current restricted attach path is absent at this call; the combined fixture asserts `try_new` refuses an attested handle. | `push_registrations` is now owner-prepared and table-ACL checked in the extra fixture; a focused missing-INSERT and extra-TRUNCATE witness rejects both changes ([coverage-gap fixture](../../server/crates/waddle-server/tests/physical_attestation/coverage_gap_gate.rs#L368-L372), [extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L160-L162), [drift witness](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L223-L258)). This still is not the runtime attachment used by startup. |
| `PushService` | Database Push Service is constructed unconditionally; disabling APNs/FCM only avoids those provider branches ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1357-L1429)). | Owner phase now prepares and checks `push_owner_locks`, `push_nodes`, `push_node_locks`, `push_devices`, `push_delivery_attempts`, `push_publish_jobs`, `push_publication_orders`, and `push_service_schema_marker` ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L71-L101)). It does not run the startup constructor as an attached R, nor establish complete function/sequence/trigger closure. |
| `Vapid` | `load_or_provision` runs when `lineage_attested`; otherwise startup constructs a placeholder signer ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1323-L1340)). No APNs/FCM provider does not remove the VAPID/Web Push path. | The fixture prepares `push_service_vapid_keys` and its schema marker with a fixture-only key, verifies the marker names an active key, and grants R read-only access ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L123-L128), [preparation](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L178-L205), [ACL list](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L103-L109)). No joint key/owner-role contract is established. |
| `NotificationOutbox` | If an owner-prepared outbox was supplied, startup reuses it; otherwise HTTP startup calls `try_new` ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1437-L1444)). Current attested `main` path supplies no outbox because it exits before server startup. | The extra phase now prepares `notification_candidates`, `notification_outbox`, `notification_outbox_lineage`, and owner state, then checks their table ACLs ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L55-L69), [owner preparation](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L172-L177)). This does not convert the startup fallback constructor to an attested attachment. |
| `NotificationActivity` | Store constructor is unconditional in HTTP startup ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1456-L1462)); current fixture demonstrates the attested owner-bootstrap refusal ([coverage-gap fixture](../../server/crates/waddle-server/tests/physical_attestation/coverage_gap_gate.rs#L365-L367)). | The extra phase owner-prepares `notification_activity` and its marker and checks their listed table ACLs ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L163-L165), [ACL list](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L47-L53)). Separate per-store attach diagnostics do not establish a joined profile. |
| `CallTeardown` | Startup has an attested `attach_existing` branch and otherwise owner constructor ([http.rs](../../server/crates/waddle-server/src/server/http.rs#L1051-L1072)). Current process-level attested startup is still blocked before this caller. | The extra fixture owner-prepares `call_teardown_outbox` and checks its listed table ACL ([extra fixture](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L166-L168), [ACL list](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L110-L114)). Related functions/dependencies and attachment in the combined profile remain unresolved. |
| `ExtensionData` | Configured extension modules are off for this candidate, so extension module dispatch is not selected. | Migration artifacts remain: `extension_grants`, `extension_room_observers`, `extension_room_observation_work`; disabling modules does not remove DDL or grants. See [migration registration/source](../../server/crates/waddle-server/src/db/migrations/waddle.rs) and [extension command paths](../../server/crates/waddle-server/src/server/extension_commands/mod.rs). |

## Established role and ACL expectations

The existing restricted role vocabulary is split across independent checks.
The combined fixture provisions a distinct database owner and schema owner plus
runtime, API, helper, migrator, installer, and definer roles
([role setup](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs#L209-L251)).
The partial pinned verifier additionally treats `pg_monitor` as a principal;
it requires independently pinned distinct role OIDs and excludes the database
owner from the other pinned roles ([pins](../../server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs#L66-L117)).
These are fixture/profile roles, not yet a complete role matrix for all enabled
callers.

| Existing verifier | Established check | Coverage boundary |
| --- | --- | --- |
| Global / Inbox / SM / pending | R must be the login and current role, with no SUPERUSER/CREATEROLE/CREATEDB/REPLICATION/BYPASSRLS, not owner or role-member, no CREATE/TEMP, and no `session_replication_role` SET. Relation ACLs require SELECT and exact configured DML, reject extra column DML, grant options, PUBLIC, TRUNCATE/REFERENCES/TRIGGER, and unexpected PUBLIC ACLs. Separate bounded checks reject unexpected rules/triggers for Inbox, SM, and pending, with one pending trigger allowlisted; pending also has a sequence rule. | Only `_lineage`, `_migrations`, the two Inbox tables, four SM tables, three pending tables, and its sequence are listed in [`runtime_store_acl.rs`](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L16-L89) and checked by [verifier SQL](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L91-L171) and [ACL SQL](../../server/crates/waddle-server/src/db/runtime_store_acl.rs#L194-L285). |
| Ingress / epoch / PubSub partial profile | Fixed OID checks for selected roles; API A has SELECT/INSERT/UPDATE/DELETE on 13 named ingress heaps. Runtime R and helper H have a 12-relation PubSub matrix, with H's UPDATE rights recorded; epoch/manifest direct ACLs are checked separately. | Direct ACL and catalog checks for the pinned subset only. The module header says it excludes columns, indexes, constraints, triggers, functions, other relations, and all database privileges ([scope](../../server/crates/waddle-server/src/db/physical_attestation/pinned_partial_profile.rs#L1-L11)). |
| Additional owner-phase fixture | R is granted the listed operation set on each of 21 tables. The query checks expected owner, R not owner/member, no column ACLs, and no unexpected table ACLs or grants. It also witnesses missing INSERT and excess TRUNCATE on `push_registrations` ([implementation](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L16-L24), [checker](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L208-L258), [query](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L261-L294)). | This is a manually maintained fixture table list, not derived from enabled callers. It does not classify routines, sequences, triggers, indexes, all role memberships, or absent/extra objects across the schema. |
| MAM / room effect / MUC fixture grants | The combined fixture grants SELECT/INSERT and selected column UPDATE rights for two MAM tables, plus `next_seq` on the archive sequence; it grants all four DML rights on room effects, and feature-gated MUC table DML ([fixture grants](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile.rs#L795-L809)). | Test setup grants are not a versioned complete expectation matrix; the clustered MUC grants are inconsistent with runtime-disabled store selection. |

`DurableStore` has only `Global`, `ControlPlane`, MAM, SM, pending, Inbox,
PubSub, spaces metadata, and channel-space links
([enum](../../server/crates/waddle-server/src/db/lineage/registry.rs#L17-L29)).
Lifecycle registers aliases for stores sharing Global and separate MAM/Control
Plane entries, but the enum is a readiness boundary registry, not a catalog of
all SQL consumers. It omits identity/auth and all of the Push, notification,
room-effect, MUC-destroy, and call-teardown stores above; it also cannot infer
which global migrated tables their callers reach.

## Open closure and conflicts

- No source-to-object closure connects each enabled caller to every table,
  sequence, index, constraint, function, trigger, view/rule, schema grant, or
  database-level privilege it can touch. Migration files and store query paths
  must both be included; `DurableStore` alone is insufficient.
- No versioned complete role graph or ACL matrix states owner, installer,
  migrator, runtime, API, helper, monitor, definer, and PUBLIC rights across the
  union of enabled objects. Per-store attachment checks are not a substitute.
- The combined PostgreSQL fixture is real. Its earlier present/absent
  observation is representative only; the new extra phase then prepares and
  ACL-checks selected spaces, links, Push, notification, and call-teardown
  tables. Space/link preparation leaves the paired marker empty, and neither
  phase checks complete missing/extra object drift. Clustered MUC objects are
  still initialized by build feature rather than runtime selection
  ([observations](../../server/crates/waddle-server/tests/physical_attestation/coverage_gap_gate.rs#L308-L433),
  [extra owner phase](../../server/crates/waddle-server/tests/physical_attestation/combined_restricted_profile_extra.rs#L116-L258)).
- Gate 1 still needs exact missing/extra object witnesses, complete all-family ACL drift witnesses,
  and an independent review. Same-transaction proof, old-backend/provider
  fencing, writer conversion, and serving activation are later gates per the
  [known-coverage plan](./2116-known-coverage-gap-gate.md#L61-L66).
