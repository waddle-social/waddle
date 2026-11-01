# P2.1 dark attestation installer split

Status: draft plan stacked on #2154. This work leaves production startup, CNPG configuration, credentials, pools and epochs unchanged.

- Require independently provisioned restricted definer and installer roles, a trusted schema and narrow grants. Audit role attributes, effective privileges and ownership before installing or invoking a pre-existing function.
- Install the exact security-definer function and its ACLs atomically with an operation UUID and immutable specification marker. Serialize attempts for the same database resource; accept a same-ID retry only after exact committed catalog verification, and refuse conflicts without replacing existing objects.
- Distinguish commit acknowledgement loss and unverified readback from a proven rollback. An absent marker does not prove that an earlier installer cannot still run; no automatic destructive repair is permitted.
- Exercise separate administrator, installer, definer and runtime logins on disposable PostgreSQL, including unexpected memberships and grants, spoofed definitions, concurrent attempts and reproducible cancellation/unknown-commit schedules. Run focused tests, strict Clippy, formatting and required-test inventory checks.

This component remains dark. Role provisioning, trusted identity handoff, authenticated transport, primary fencing, startup wiring and runtime activation require separate review and operator procedures.
