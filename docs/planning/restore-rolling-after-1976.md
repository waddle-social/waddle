# Restore rolling updates after the resume ACK journal cutover

PR #1976 introduces durable ACK intervals in `sm_acknowledged_windows`.
Migration V1027 creates this table under the serialized migration lock before
replicas initialize their stores. Older migration catalogs reject V1027 at
startup, but this does not stop an already-running older replica.
Writers and cleanup workers must understand this evidence before sharing retained
SM sources: an older binary can delete a source or release its pending rows
without settling the recorded ACK. The GitOps release therefore uses the existing
`Recreate` cutover mechanism for this deployment.

Restore `RollingUpdate` in a separate change after verifying that every ready
server replica runs the ACK-journal implementation. Keep the publisher-derived
cutover guard enabled; follow [the rollout procedure](../operations/relay-cutovers.md).
The rolling-strategy change must not include additional server changes, so the
guard can attest that the preceding source revision reached the fleet.

Before a rollback to a binary without the journal reader, stop new resume
admission and settle all retained windows with the current implementation. Do not
delete journal rows as cleanup: they are the evidence preventing replay of
acknowledged pending messages. A failed settlement keeps the source and its
pending-row claims retained for retry. A binary whose catalog predates V1027
will also reject the migration ledger; rollback needs a deliberate catalog-aware
procedure after settlement, rather than deleting its ledger entry or table.
