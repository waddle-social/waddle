// PObserve runtime monitor for bounded stream permits,
// unknown_after_send quarantine (never retrying quarantined streams),
// and valid Drop / ReplicaFence stream cancellation ordering.

spec StreamCancellationMonitor observes eStreamOpen, eStreamCancelReq, eStreamAbortDone, eStreamQuarantine, eStreamClose {
    var openStreams: set[string];
    var quarantinedStreams: set[string];
    var cancelReqTick: map[string, int];

    start state Monitoring {
        on eStreamOpen do (payload: (tenant: string, streamId: string, permitsAfter: int, maxPermits: int)) {
            assert !(payload.streamId in quarantinedStreams),
                "quarantined stream must never be retried or reopened";
            assert payload.permitsAfter <= payload.maxPermits,
                "stream concurrency permit bound exceeded";
            openStreams += (payload.streamId);
        }

        on eStreamCancelReq do (payload: (tenant: string, streamId: string, tick: int)) {
            cancelReqTick[payload.streamId] = payload.tick;
        }

        on eStreamAbortDone do (payload: (tenant: string, streamId: string, tick: int, permitsAfter: int)) {
            assert payload.streamId in openStreams,
                "stream abort completed for non-open stream";
            assert payload.streamId in cancelReqTick,
                "stream abort completed without prior cancel request";
            assert payload.tick >= cancelReqTick[payload.streamId],
                "stream abort tick precedes cancel request tick";
            openStreams -= (payload.streamId);
            cancelReqTick -= (payload.streamId);
        }

        on eStreamQuarantine do (payload: (tenant: string, streamId: string, permitsAfter: int)) {
            assert payload.streamId in openStreams,
                "stream quarantine on non-open stream";
            openStreams -= (payload.streamId);
            quarantinedStreams += (payload.streamId);
        }

        on eStreamClose do (payload: (tenant: string, streamId: string, permitsAfter: int)) {
            if (payload.streamId in openStreams) {
                openStreams -= (payload.streamId);
            }
            if (payload.streamId in cancelReqTick) {
                cancelReqTick -= (payload.streamId);
            }
        }
    }
}
