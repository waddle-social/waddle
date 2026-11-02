# P2.1 shared correctness facade laboratory

This test-support-only facade is unwired. It binds one database pool's main
lane to an archive lookup and its reserved ingress lane to the existing
submission operation. A PostgreSQL facade requires the exact attested pool,
the configured reserved lane, and a matching lineage deployment. SQLite uses
the actual supplied pool; any in-memory pool must have exactly one connection,
even if configured for shared cache.
The facade returns no database handle, raw connection, transaction, or callback.

Submission only accepts ephemeral stream identity and requires an origin ID; it delegates the current
generic planning contract, so its principal and complete plan must come from
trusted ingress planning. It does not validate that arbitrary submissions are
direct-message-only or prove a caller's plan. It does not replace the lifecycle
admission and stream-retirement guards of `IngressAuthority`. The MAM lookup uses the stanza's assigning JID, the
attested schema on PostgreSQL, and `main.mam_messages` on SQLite. Schema
bootstrap is performed separately by the owner in the laboratory.

The test-only lost-ack injection runs after an acknowledged database commit;
it checks non-advancing ambiguity and a successful same-origin, same-payload
replay in that controlled schedule. A conflicting existing alias can still
mint a fresh denial key on each retry. Requiring an origin ID is not a general
commit-uncertainty barrier and does not authorize automatic retry. The test
does not characterize a delayed or still-running backend. Cancellation is not
evidence of rollback.

The facade does not close other public `Database` or legacy store paths, audit
all runtime grants or security-definer functions, fence old sessions or an old
primary, provision production roles, or authorize a startup, writer, epoch, or
custody cutover. Existing MAM owner initialization still performs autocommit
schema work and must be separated from runtime attachment before activation.
Shared capacity and MAM/main fairness require an explicit deployment policy;
the disposable one-slot lane test does not establish production sizing.
The fixture's narrow runtime grants do not authorize every generic ingress
path: for example, a concurrent first-alias race can require deleting an
uncommitted candidate message. The fixture does not grant that DELETE and its
sequential replay does not prove the competing-alias path.
