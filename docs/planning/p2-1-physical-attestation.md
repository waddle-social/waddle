# P2.1 physical PostgreSQL connection attestation

This is the first implementation slice of [#2116](https://github.com/waddle-social/waddle/issues/2116) under [#2046](https://github.com/waddle-social/waddle/issues/2046). It establishes a capability that can reject a physical PostgreSQL connection whose authenticated login or durable database identity differs from the expected installation. It does not change startup pools, transaction authority, SQLite, or production admission.

## Contract for this slice

- The installer owns a fresh administrative connection. It creates a trusted-schema function with a restricted `NOLOGIN` definer, grants only the required execution privilege, and commits before reporting an installed identity. A separate fresh session reads the committed function. An uncertain commit is an unknown outcome, not evidence that installation succeeded.
- The function uses qualified object references and a secure search path with `pg_temp` last. Creation and revocation of `PUBLIC` execution happen in one transaction. Its result binds deployment UUID, lineage UUID, PostgreSQL system identifier, database OID and name, and schema OID and name.
- A pool hook checks the configured login, `session_user`, `current_user`, and the complete typed identity on the same physical connection before it can be lent to a caller. Failure closes that connection. The candidate uses `min_connections = 0` so a failing hook cannot leave a minimum-connection maintenance loop running after close.
- The component is dark: normal startup does not install the function or construct an attested pool. A later slice must resolve owner-only bootstrap, role grants, existing schema mutation, epoch transition, pool capacity, and every raw-pool escape before using it as correctness authority.

## Required evidence

Use disposable PostgreSQL with a restricted runtime role. Exercise a committed installer and readback; missing function, revoked execution, wrong login, deployment, lineage, system identifier, database and schema; pool expansion; and replacement of a terminated backend. Verify that no rejected physical session is lent to an operation. Check SQLite remains unchanged. Record any timeout where PostgreSQL continues work after the caller stops as an availability limit, not cancellation proof.

The merged [cancelled-`BEGIN` regression](https://github.com/waddle-social/waddle/pull/2117) covers the SQLx 0.8.6 driver prerequisite. It does not establish this attestation contract or P2.1 transaction authority.
