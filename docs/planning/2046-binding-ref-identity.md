# Dark binding attempt identity

`ConnectionRef` remains the serializable physical-socket identity introduced by
#2150. Its node lease records a clustered incarnation or the standalone process
incarnation; it is equality evidence, not authority. `BindingRef` adds a fresh
attempt UUID to that physical reference. It has no embedded counter or allocator,
so cloning and deserializing it preserve an attempt rather than minting one.

A resume attempt captures its binding identity before asynchronous claim work;
failure discards it. Fresh bind captures a new identity, while successful resume
registration accepts only the attempt belonging to that same socket. Registration
takes pending identity before early exits and restores it only at publication.
The local registry entry retains the exact identity alongside its independent
`LocalConnectionBindingId`, which continues to fence local registry and actor
ownership.

The existing v4 remote registration snapshot serializes the source physical
`ConnectionRef`, not `BindingRef`. A remote-hosted mirror therefore retains that
physical source and mints its own local owner ID, but records no source binding
attempt. Propagating the latter across the wire requires a separately reviewed,
versioned protocol change. This slice changes no remote wire format.

These identities do not fence a final enqueue, activate a provisional endpoint,
repair same-socket rollback, authorize force-detach, or establish remote frame
custody. Those behaviors remain separate work.
