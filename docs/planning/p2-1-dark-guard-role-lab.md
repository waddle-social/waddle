# P2.1 dark guard and role-boundary laboratory

Status: draft plan, stacked on #2153. This work does not register or run a migration at normal startup, change runtime credentials or deployment manifests, or activate an epoch.

## Plan

- Develop a candidate append-only PostgreSQL guard as an isolated artifact. It uses the actual schema-qualified epoch relation as a transaction barrier, reads the epoch after the barrier, rejects non-READ-COMMITTED guarded writes, and verifies that a transition already holds an exclusive epoch lock.
- Exercise the actual V1008–V1027 schema and all manifest-listed guards with separate owner and runtime login roles. Cover concurrent writers and transitions, stale proofs, snapshot isolation, savepoints, shadowed names, privilege denials, and bounded failure paths.
- Add an attested-schema-bound lexical prelock only if it can be isolated without altering normal runtime. Separate privileged role provisioning from physical attestation installation only if a safe, reviewable extraction preserves its existing contract.
- Run focused real PostgreSQL tests, formatting, strict Clippy and the required-test inventory. Independently review the resulting diff and report what remains untested.

The existing migration runner, old-session cutoff, runtime bootstrap, role provisioning and all production wiring remain separate gates. A candidate SQL artifact and disposable tests do not complete P2.1 or authorize deployment or epoch activation.
