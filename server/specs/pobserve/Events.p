// Secret-free runtime trace event declarations for Waddle's PObserve monitors.
// Mirrors `server/crates/waddle-broker-core/src/trace_verifier.rs`.
// Contains NO prompts, responses, API keys, ciphertexts, auth tokens, or raw JIDs.

event eFenceClose: (tenant: string, fenceKind: string, newGen: int);
event eFenceReopen: (tenant: string, fenceKind: string, gen: int);
event eReplicaFenceAck: (tenant: string, replica: string, gen: int);
event eRevokeAck: (tenant: string, gen: int);
event eAdmissionCommit: (
    tenant: string,
    attemptId: string,
    payer: string,
    requestedPayer: string,
    credGen: int,
    roomGen: int,
    placeGen: int
);
event eProviderDispatch: (tenant: string, attemptId: string, credGen: int);

event eLeaseClaim: (tenant: string, holderKey: string, gen: int);
event eLeaseTransfer: (tenant: string, fromKey: string, toKey: string, newGen: int);
event eLeaseRelease: (tenant: string, holderKey: string, gen: int);
event eTenantWrite: (tenant: string, writerKey: string, gen: int);

event eStreamOpen: (tenant: string, streamId: string, permitsAfter: int, maxPermits: int);
event eStreamCancelReq: (tenant: string, streamId: string, tick: int);
event eStreamAbortDone: (tenant: string, streamId: string, tick: int, permitsAfter: int);
event eStreamQuarantine: (tenant: string, streamId: string, permitsAfter: int);
event eStreamClose: (tenant: string, streamId: string, permitsAfter: int);
