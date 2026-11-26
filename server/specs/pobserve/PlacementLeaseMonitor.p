// PObserve runtime monitor for PlacementLeaseStore monotonic generations,
// valid CAS handoffs, and strict write fencing per incarnation key.

spec PlacementLeaseMonitor observes eLeaseClaim, eLeaseTransfer, eLeaseRelease, eTenantWrite {
    var activeHolder: map[string, string];
    var activeGen: map[string, int];

    start state Monitoring {
        on eLeaseClaim do (payload: (tenant: string, holderKey: string, gen: int)) {
            if (payload.tenant in activeGen) {
                assert payload.gen > activeGen[payload.tenant],
                    "lease claim generation must strictly increase";
            }
            activeHolder[payload.tenant] = payload.holderKey;
            activeGen[payload.tenant] = payload.gen;
        }

        on eLeaseTransfer do (payload: (tenant: string, fromKey: string, toKey: string, newGen: int)) {
            assert payload.tenant in activeHolder,
                "lease transfer requires an active holder";
            assert activeHolder[payload.tenant] == payload.fromKey,
                "lease transfer fromKey must match current holder";
            assert payload.newGen > activeGen[payload.tenant],
                "lease transfer generation must strictly increase";
            activeHolder[payload.tenant] = payload.toKey;
            activeGen[payload.tenant] = payload.newGen;
        }

        on eLeaseRelease do (payload: (tenant: string, holderKey: string, gen: int)) {
            if (payload.tenant in activeHolder) {
                if (activeHolder[payload.tenant] == payload.holderKey && activeGen[payload.tenant] == payload.gen) {
                    activeHolder -= (payload.tenant);
                }
            }
        }

        on eTenantWrite do (payload: (tenant: string, writerKey: string, gen: int)) {
            assert payload.tenant in activeHolder,
                "tenant write without active placement lease";
            assert activeHolder[payload.tenant] == payload.writerKey,
                "tenant write from non-holder or stale incarnation key";
            assert activeGen[payload.tenant] == payload.gen,
                "tenant write with stale placement generation";
        }
    }
}
