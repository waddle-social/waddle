import Testing
import WaddleKit

struct CallLifecycleTrackerTests {
    private let instant = ContinuousClock.now
    private var outgoing: CallState { .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio)) }
    private var incoming: CallState { .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)) }
    private var joining: CallState {
        .joiningGroup(GroupCallAttempt(room: callPeer.bare, sid: "c1", media: callAudio, nick: "alice"))
    }
    private func ended(_ reason: CallEndReason) -> CallState { .ended(sid: "c1", reason: reason) }

    @Test func terminalMappingBeforeAndAfterMediaConnection() {
        typealias Row = (CallState, CallState, CallLifecycleTracker.Exit?, CallLifecycleReport.SetupOutcome, CallLifecycleReport.EndReason)
        let rows: [Row] = [
            (outgoing, .idle, .hangUp, .proposed, .hangup),
            (incoming, .idle, .hangUp, .declined, .hangup),
            (outgoing, .idle, .cleared, .proposed, .hangup),
            (incoming, .idle, .cleared, .declined, .hangup),
            (incoming, .idle, .decline, .declined, .hangup),
            (incoming, .idle, .remote(own: true), .proposed, .hangup),
            (outgoing, ended(.rejected), .remote(own: false), .declined, .peerLeft),
            (incoming, ended(.rejected), .remote(own: true), .declined, .hangup),
            (incoming, ended(.expired), .remote(own: true), .declined, .hangup),
            (outgoing, ended(.retracted), .remote(own: false), .proposed, .peerLeft),
            (outgoing, ended(.expired), nil, .proposed, .peerLeft),
            (incoming, ended(.missed), nil, .proposed, .peerLeft),
            (outgoing, ended(.timeout), nil, .timeout, .error),
            (outgoing, ended(.error), nil, .failed, .error),
            (outgoing, .idle, .mediaFailure, .failed, .error),
            (outgoing, .incoming(IncomingCall(from: callPeer, sid: "replacement", media: callAudio)), nil, .proposed, .hangup),
            (joining, .idle, nil, .failed, .error),
            (joining, ended(.finished(.success)), .remote(own: false), .failed, .error),
            (joining, .idle, .hangUp, .proposed, .hangup)
        ]
        for (from, to, cause, setup, end) in rows {
            for connected in [false, true] {
                var tracker = CallLifecycleTracker()
                tracker.begin(sid: "c1", kind: .direct)
                if connected { tracker.mediaConnected(sid: "c1", at: instant) }
                let report = tracker.finish(sid: "c1", from: from, to: to, cause: cause, at: instant)
                #expect(report?.setupOutcome == (connected ? .accepted : setup))
                let expectedEnd: CallLifecycleReport.EndReason = connected && to.isLive ? .peerLeft : end
                #expect(report?.endReason == expectedEnd)
                #expect(tracker.finish(sid: "c1", from: from, to: to, cause: cause, at: instant) == nil)
            }
        }
    }

    @Test(arguments: [false, true])
    func sessionEndedByTheServerIsThePeerLeaving(connected: Bool) {
        var tracker = CallLifecycleTracker()
        tracker.begin(sid: "c1", kind: .direct)
        if connected {
            tracker.mediaConnected(sid: "c1", at: instant)
            tracker.mediaReconnecting(sid: "c1")
        }
        let report = tracker.finish(
            sid: "c1", from: outgoing, to: ended(.finished(.connectivityError)), cause: .endedByServer, at: instant)
        #expect(report?.setupOutcome == (connected ? .accepted : .proposed))
        #expect(report?.endReason == .peerLeft)
    }

    @Test func everyJingleReasonHasAnExplicitMapping() {
        for reason in JingleReason.allCases {
            let setup: CallLifecycleReport.SetupOutcome
            let end: CallLifecycleReport.EndReason
            switch reason {
            case .decline, .busy: (setup, end) = (.declined, .peerLeft)
            case .success, .cancel, .alternativeSession, .expired: (setup, end) = (.proposed, .peerLeft)
            case .timeout: (setup, end) = (.timeout, .error)
            // The failing device reports its own failure; here the other
            // side simply left.
            case .connectivityError, .failedApplication, .failedTransport, .generalError, .gone,
                .incompatibleParameters, .mediaError, .securityError, .unsupportedApplications, .unsupportedTransports:
                (setup, end) = (.proposed, .peerLeft)
            }
            for connected in [false, true] {
                for own in [false, true] {
                    var tracker = CallLifecycleTracker()
                    tracker.begin(sid: "c1", kind: .direct)
                    if connected { tracker.mediaConnected(sid: "c1", at: instant) }
                    let report = tracker.finish(
                        sid: "c1", from: outgoing, to: ended(.finished(reason)), cause: .remote(own: own), at: instant)
                    #expect(report?.setupOutcome == (connected ? .accepted : setup))
                    let ownHangup = own && (setup == .declined || (connected && setup == .proposed))
                    // A remote timer that ran out on a connected call is the peer leaving.
                    let remoteTimeout = connected && reason == .timeout
                    #expect(report?.endReason == (ownHangup ? .hangup : remoteTimeout ? .peerLeft : end))
                    // No reason the other side can send makes this device report a failure or a drop.
                    #expect(report?.setupOutcome != .failed)
                    if connected { #expect(report?.endReason != .error && report?.endReason != .reconnectExhausted) }
                }
            }
        }
        for connected in [false, true] {
            for own in [false, true] {
                for reason: CallEndReason in [.finished(nil), .retracted, .expired, .missed] {
                    var tracker = CallLifecycleTracker()
                    tracker.begin(sid: "c1", kind: .direct)
                    if connected { tracker.mediaConnected(sid: "c1", at: instant) }
                    let report = tracker.finish(sid: "c1", from: outgoing, to: ended(reason), cause: .remote(own: own), at: instant)
                    #expect(report?.setupOutcome == (connected ? .accepted : .proposed))
                    #expect(report?.endReason == (connected && own ? .hangup : .peerLeft))
                }
            }
        }
    }

    @Test func unknownDuplicateAndAlreadyReportedSessionsAreIgnored() {
        var tracker = CallLifecycleTracker()
        #expect(tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: nil, at: instant) == nil)
        tracker.begin(sid: "", kind: .direct)
        #expect(tracker.finish(sid: "", from: outgoing, to: .idle, cause: nil, at: instant) == nil)
        tracker.mediaConnected(sid: "unknown", at: instant)
        tracker.mediaReconnecting(sid: "unknown")
        tracker.mediaRecovered(sid: "unknown")
        tracker.begin(sid: "c1", kind: .group)
        tracker.mediaConnected(sid: "c1", at: instant)
        tracker.begin(sid: "c1", kind: .direct)
        let report = tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: .hangUp, at: instant)
        #expect(report?.kind == .group)
        #expect(report?.setupOutcome == .accepted)
        tracker.begin(sid: "c1", kind: .direct)
        #expect(tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: .hangUp, at: instant) == nil)
    }

    @Test func durationUsesFirstMediaConnectionAndEveryBoundary() {
        let boundaries: [(Duration, CallLifecycleReport.DurationBucket)] = [
            (.seconds(-1), .none), (.zero, .none), (.nanoseconds(1), .underOneMinute),
            (.seconds(60) - .nanoseconds(1), .underOneMinute), (.seconds(60), .oneToTenMinutes),
            (.seconds(600) - .nanoseconds(1), .oneToTenMinutes), (.seconds(600), .tenToSixtyMinutes),
            (.seconds(3600) - .nanoseconds(1), .tenToSixtyMinutes), (.seconds(3600), .overSixtyMinutes),
            (.seconds(7200), .overSixtyMinutes)
        ]
        for (elapsed, bucket) in boundaries {
            var tracker = CallLifecycleTracker()
            tracker.begin(sid: "c1", kind: .direct)
            tracker.mediaConnected(sid: "c1", at: instant)
            tracker.mediaConnected(sid: "c1", at: instant.advanced(by: .seconds(40)))
            let report = tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: .hangUp, at: instant.advanced(by: elapsed))
            #expect(report?.duration == bucket)
        }
        var tracker = CallLifecycleTracker()
        tracker.begin(sid: "c1", kind: .direct)
        #expect(tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: .hangUp, at: instant.advanced(by: .seconds(7200)))?.duration == CallLifecycleReport.DurationBucket.none)
    }

    @Test func reconnectCountsAndRecoveryControlFailureReason() {
        for count in [0, 1, 2, 3] {
            for recover in [false, true] {
                for connected in [false, true] {
                    var tracker = CallLifecycleTracker()
                    tracker.begin(sid: "c1", kind: .direct)
                    if connected { tracker.mediaConnected(sid: "c1", at: instant) }
                    for _ in 0..<count { tracker.mediaReconnecting(sid: "c1") }
                    if recover { tracker.mediaRecovered(sid: "c1") }
                    let report = tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: .mediaFailure, at: instant)
                    let bucket: CallLifecycleReport.ReconnectBucket = count == 0 ? .none : count == 1 ? .once : .multiple
                    #expect(report?.reconnects == bucket)
                    #expect(report?.endReason == (connected && count > 0 && !recover ? .reconnectExhausted : .error))
                    #expect(report?.setupOutcome == (connected ? .accepted : .failed))
                }
            }
        }
        var tracker = CallLifecycleTracker()
        tracker.begin(sid: "c1", kind: .direct)
        tracker.mediaReconnecting(sid: "c1")
        tracker.mediaConnected(sid: "c1", at: instant)
        #expect(tracker.finish(sid: "c1", from: outgoing, to: .idle, cause: .mediaFailure, at: instant)?.endReason == .error)
    }

    @Test func reportedMemoryEvictsInInsertionOrderAt256() {
        var tracker = CallLifecycleTracker()
        for index in 0..<257 {
            #expect(tracker.declinedUnseen(sid: "sid-\(index)") != nil)
        }
        #expect(tracker.declinedUnseen(sid: "sid-1") == nil)
        tracker.begin(sid: "sid-0", kind: .group)
        #expect(tracker.finish(sid: "sid-0", from: outgoing, to: .idle, cause: .hangUp, at: instant)?.kind == .group)
        #expect(tracker.declinedUnseen(sid: "sid-2") == nil)
        #expect(tracker.declinedUnseen(sid: "sid-1") != nil)
    }

    @Test func unseenDeclineOnlyReportsUntrackedSessionsOnce() {
        var tracker = CallLifecycleTracker()
        tracker.begin(sid: "c1", kind: .direct)
        #expect(tracker.declinedUnseen(sid: "c1") == nil)
        let report = tracker.declinedUnseen(sid: "push")
        #expect(report == CallLifecycleReport(kind: .direct, setupOutcome: .declined, endReason: .hangup, duration: .none, reconnects: .none))
        #expect(tracker.declinedUnseen(sid: "push") == nil)
        tracker.begin(sid: "push", kind: .direct)
        #expect(tracker.finish(sid: "push", from: incoming, to: .idle, cause: .decline, at: instant) == nil)
    }
}
