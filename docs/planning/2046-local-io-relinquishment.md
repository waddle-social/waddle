# P3.1 local I/O relinquishment witness

The production connection future owns both halves of its physical WebSocket
inside `LocalSocketIo`. Its read-only witness is carried by `WsConnState` and
copied into locally published `ConnectionEntry` instances alongside their
existing `ConnectionRef` and `BindingRef`. Different binding attempts on one
socket share the witness. Entries without a local transport owner, including
remote mirrors, have no witness.

The owner reports relinquishment only after both half destructors return. This
also applies when the connection future is cancelled or unwinds. If a half
destructor panics, the observer reports the outcome as unobserved rather than
claiming success. The witness does not establish task completion, successful
cleanup, a close handshake, peer behavior, write delivery, or anything after a
process crash.

On normal connection shutdown, the final best-effort socket writes and
pre-cleanup inbound and relay settlement run first. Both halves are then
dropped and relinquishment is published before persistence and socket lifecycle
cleanup. The shutdown-owning force-detach request is still acknowledged after
cleanup with its actual outcome; rejection and retry responses are unchanged.

`SocketCleanupState` still describes binding cleanup. A failed resume may
detach its binding while the physical witness remains pending and the same
socket starts a new binding. The witness is not an admission, routing,
occupancy, stream-management, or cross-node authority gate. In particular,
fresh-bind occupancy publication and incumbent cleanup retain their existing
ordering. Associating a witness with an entry depends on the local transport
owner supplying the observer for its own halves; this observation alone is not
an authorization mechanism.

## Dark split-pump witness

The inactive supervised-pump path uses a separate witness attached to the
custodian observation for the exact `ConnectionRef`. After its factory returns,
the supplied reader and writer are each wrapped before they move into pump
futures. Each wrapper publishes its own marker only after its owned half's
destructor returns; both markers are required for `Relinquished`. A panic in
either destructor therefore cannot publish false success.

A generic factory can return wrappers that do not exclusively own a physical
WebSocket. This witness proves the supplied values were dropped; activation
must bind the owner and move the real exclusive socket halves into the pumps.

A reservation whose factory has not returned both halves has no witness;
inventory observation is unavailable, including when startup ends in that
phase. After a witness exists, `Unobserved` requires both marker producers to
be gone and startup plus every started pump to reach terminal join proof.
Without both drop markers, a lost reaper or noncooperative half remains
pending. A successful two-half drop can be observed before unrelated auxiliary
cleanup completes; it does not assert that cleanup or task joins succeeded.
The inventory retains its existing
`CleanupPending` entries until the actual joins complete.

This dark witness does not activate the supervised pumps for `/ws`, alter the
active connection path, or change force-detachment and persistence ordering.
