# P2.1 physical PostgreSQL connection attestation

This is the first implementation slice of [#2116](https://github.com/waddle-social/waddle/issues/2116) under [#2046](https://github.com/waddle-social/waddle/issues/2046). It establishes a capability that can reject a physical PostgreSQL connection whose authenticated login or durable database identity differs from the expected installation. It does not change startup pools, transaction authority, SQLite, or production admission.

## Contract for this slice

- The installer owns a fresh administrative connection. It creates a trusted-schema function with a restricted `NOLOGIN` definer, checks the effective function and schema grants before committing, and grants only the required runtime access. A separate fresh session reads the committed function and grants. A failed COMMIT has an unknown outcome; a failed readback leaves a committed but unverified installation. Both require inspection before retry.
- The function uses qualified object references and a secure search path with `pg_temp` last. Creation and revocation of `PUBLIC` execution happen in one transaction. Its result binds deployment UUID, lineage UUID, PostgreSQL system identifier, database OID and name, and schema OID and name.
- Pool hooks check the configured login, `session_user`, `current_user`, and the complete typed identity on the same physical connection when it is created and before each later checkout. An idle checkout first rolls back any raw transaction left by a previous borrower, so a pinned snapshot cannot attest stale lineage. Failure closes that connection. This extra round trip needs capacity measurement before activation. The candidate uses `min_connections = 0` so a failing hook cannot leave a minimum-connection maintenance loop running after close.
- The component is dark: normal startup does not install the function or construct an attested pool. A later slice must resolve owner-only bootstrap, role grants, existing schema mutation, epoch transition, pool capacity, and every raw-pool escape before using it as correctness authority.

## Required evidence

Use disposable PostgreSQL with a restricted runtime role. Exercise a committed installer and readback; missing function, revoked execution, unexpected or grantable default privileges, wrong login, deployment, lineage, system identifier, database and schema; pool expansion; replacement of a terminated backend; and rejection of reused sessions changed by `SET ROLE`, `SET search_path`, or a raw transaction with a pinned snapshot. Verify that a spoofed schema function cannot replace PostgreSQL's identity functions and that no rejected physical session is lent to an operation. Check SQLite remains unchanged. Record any timeout where PostgreSQL continues work after the caller stops as an availability limit, not cancellation proof.

The merged [cancelled-`BEGIN` regression](https://github.com/waddle-social/waddle/pull/2117) covers the SQLx 0.8.6 driver prerequisite. It does not establish this attestation contract or P2.1 transaction authority.
