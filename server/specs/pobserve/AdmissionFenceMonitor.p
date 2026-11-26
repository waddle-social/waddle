// PObserve runtime monitor for 3-fence admission, strict fence monotonicity,
// at-most-once provider dispatch, payer separation, and post-revoke-ack safety.

spec AdmissionFenceMonitor observes eFenceClose, eRevokeAck, eAdmissionCommit, eProviderDispatch {
    var credFenceGen: map[string, int];
    var roomFenceGen: map[string, int];
    var placeFenceGen: map[string, int];
    var credRevokedGen: map[string, int];
    var dispatchedAttempts: set[string];

    start state Monitoring {
        on eFenceClose do (payload: (tenant: string, fenceKind: string, newGen: int)) {
            if (payload.fenceKind == "cred") {
                if (payload.tenant in credFenceGen) {
                    assert payload.newGen > credFenceGen[payload.tenant],
                        "cred fence generation must strictly increase";
                }
                credFenceGen[payload.tenant] = payload.newGen;
            } else if (payload.fenceKind == "room") {
                if (payload.tenant in roomFenceGen) {
                    assert payload.newGen > roomFenceGen[payload.tenant],
                        "room fence generation must strictly increase";
                }
                roomFenceGen[payload.tenant] = payload.newGen;
            } else if (payload.fenceKind == "placement") {
                if (payload.tenant in placeFenceGen) {
                    assert payload.newGen > placeFenceGen[payload.tenant],
                        "placement fence generation must strictly increase";
                }
                placeFenceGen[payload.tenant] = payload.newGen;
            }
        }

        on eRevokeAck do (payload: (tenant: string, gen: int)) {
            if (payload.tenant in credRevokedGen) {
                if (payload.gen > credRevokedGen[payload.tenant]) {
                    credRevokedGen[payload.tenant] = payload.gen;
                }
            } else {
                credRevokedGen[payload.tenant] = payload.gen;
            }
        }

        on eAdmissionCommit do (payload: (
            tenant: string,
            attemptId: string,
            payer: string,
            requestedPayer: string,
            credGen: int,
            roomGen: int,
            placeGen: int
        )) {
            assert payload.payer == payload.requestedPayer,
                "silent payer fallback is forbidden";
            assert !(payload.attemptId in dispatchedAttempts),
                "attempt already dispatched";
        }

        on eProviderDispatch do (payload: (tenant: string, attemptId: string, credGen: int)) {
            assert !(payload.attemptId in dispatchedAttempts),
                "at-most-once provider dispatch violated";
            if (payload.tenant in credRevokedGen) {
                assert payload.credGen >= credRevokedGen[payload.tenant],
                    "post-revoke-ack dispatch from pre-revoke credential generation";
            }
            dispatchedAttempts += (payload.attemptId);
        }
    }
}
