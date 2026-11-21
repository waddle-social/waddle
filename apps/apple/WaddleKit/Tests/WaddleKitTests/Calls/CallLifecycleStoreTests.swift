import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Call lifecycle observations") struct CallLifecycleStoreTests {
    @Test func callerHangUpBeforeAnswerIsProposed() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        await f.store.hangUp()
        expectReports(f, [report(.proposed, .hangup)])
    }

    @Test func calleeDeclineIsDeclined() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        #expect(await f.store.declineIncoming())
        expectReports(f, [report(.declined, .hangup)])
    }

    @Test(arguments: [false, true])
    func siblingChoiceDistinguishesAnswerFromReject(reject: Bool) async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        let sibling = JID(parsing: "alice@waddle.test/tablet")!
        f.store.handle(f.event(reject ? .reject(reason: nil, tieBreak: false) : .proceed, from: sibling))
        expectReports(f, [report(reject ? .declined : .proposed, .hangup)])
    }

    @Test func callerRingTimeoutIsTimeout() async {
        let f = CallFixture(ringTimeout: .milliseconds(20))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.timeout, .error)])
    }

    @Test func unansweredCalleeIsProposed() async {
        let f = CallFixture(unansweredTimeout: .milliseconds(20))
        f.store.handle(f.event(.propose(callAudio)))
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.proposed, .peerLeft)])
    }

    @Test(arguments: [false, true])
    func sessionTimeoutIsTimeout(incoming: Bool) async {
        let f = CallFixture(sessionTimeout: .milliseconds(20))
        if incoming {
            f.store.handle(f.event(.propose(callAudio)))
            #expect(await f.store.acceptIncoming())
        } else {
            #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
            f.store.handle(f.event(.proceed))
        }
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.timeout, .error)])
    }

    @Test func peerRejectIsDeclinedAndPeerLeft() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false)))
        f.store.handle(f.event(.finish(reason: .decline, migratedTo: nil)))
        expectReports(f, [report(.declined, .peerLeft)])
    }

    @Test func signalingAcceptanceAloneDoesNotMeanMediaConnected() async {
        let f = CallFixture()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        await f.store.hangUp()
        expectReports(f, [report(.proposed, .hangup)])
    }

    @Test func failedMediaConnectIsFailed() async {
        let media = RecordingCallMedia()
        media.connectThrows = true
        let f = CallFixture(media: media)
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.failed, .error)])
    }

    @Test func localHangUpAfterMediaConnectUsesMediaDuration() async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        f.now = f.now.advanced(by: .seconds(60))
        await f.store.hangUp()
        expectReports(f, [report(.accepted, .hangup, duration: .oneToTenMinutes)])
    }

    @Test func peerNormalEndAfterMediaConnectIsPeerLeft() async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        f.now = f.now.advanced(by: .seconds(5))
        f.store.handle(f.event(.sessionTerminate(reason: .success)))
        f.store.handle(f.event(.finish(reason: .success, migratedTo: nil)))
        expectReports(f, [report(.accepted, .peerLeft, duration: .underOneMinute)])
    }

    @Test(arguments: [false, true])
    func mediaFailureDistinguishesReconnectExhaustion(reconnecting: Bool) async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        f.now = f.now.advanced(by: .seconds(5))
        if reconnecting { media.onPhaseChange?(.reconnecting) }
        media.onFailure?()
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.accepted, reconnecting ? .reconnectExhausted : .error,
                                 duration: .underOneMinute, reconnects: reconnecting ? .once : .none)])
    }

    @Test func recoveredReconnectDoesNotCountAsExhaustion() async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        media.onPhaseChange?(.reconnecting)
        media.onPhaseChange?(.connected)
        media.onFailure?()
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.accepted, .error, reconnects: .once)])
    }

    @Test func ignoredPhasesDoNotEraseReconnectInProgress() async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        media.onPhaseChange?(.reconnecting)
        media.onPhaseChange?(.idle)
        media.onPhaseChange?(.connecting)
        media.onFailure?()
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.accepted, .reconnectExhausted, reconnects: .once)])
    }

    @Test(arguments: [JingleReason.success, .connectivityError])
    func remoteActiveEndKeepsItsOwnCauseWhileReconnecting(reason: JingleReason) async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        media.onPhaseChange?(.reconnecting)
        await f.store.hangUpActive(ifSid: "c1", reason: reason)
        // The peer's failure is the peer's to report: here it only left.
        expectReports(f, [report(.accepted, .peerLeft, reconnects: .once)])
    }

    @Test func sessionEndedByTheServerIsNotAMediaFailure() async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        // The other side hung up: the SFU removed this device before the
        // terminate arrived.
        media.onPhaseChange?(.reconnecting)
        media.wasEndedByServer = true
        media.onFailure?()
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.accepted, .peerLeft, reconnects: .once)])
    }

    @Test func staleMediaCallbacksCannotChangeReplacementOutcome() async throws {
        let media = RecordingCallMedia()
        var number = 0
        let f = CallFixture(sid: { number += 1; return "c\(number)" }, media: media)
        await connect(f, media: media)
        let oldPhaseChange = try #require(media.onPhaseChange)
        let oldFailure = try #require(media.onFailure)
        await f.store.hangUp()
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: "c2"))
        await eventually { media.calls.filter { $0 == .connect(callJoin, callAudio) }.count == 2 }
        oldPhaseChange(.reconnecting)
        oldFailure()
        media.onFailure?()
        await eventually { f.reports.count == 2 }
        expectReports(f, [report(.accepted, .hangup), report(.accepted, .error)])
    }

    @Test func connectReturningAfterHangUpCannotAcceptTheCall() async {
        let media = RecordingCallMedia()
        media.holdConnect = true
        let f = CallFixture(media: media)
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio)))
        await eventually { media.calls.count == 1 }
        media.onPhaseChange?(.connected)
        await f.store.hangUp()
        media.releaseConnect()
        await eventually { media.calls.filter { $0 == .disconnect }.count == 2 }
        expectReports(f, [report(.proposed, .hangup)])
    }

    @Test func migrationReportsBothAdoptedSessionsOnce() async {
        let media = RecordingCallMedia()
        let f = CallFixture(sid: { "c-old" }, media: media)
        await connect(f, media: media, sid: "c-old")
        f.store.handle(f.event(.propose(callAudio), sid: "c-new"))
        await eventually { f.store.state.sid == "c-new" }
        #expect(f.reports == [report(.accepted, .peerLeft)])
        await f.store.hangUp()
        f.store.handle(f.event(.sessionTerminate(reason: .success), sid: "c-old"))
        f.store.handle(f.event(.sessionTerminate(reason: .success), sid: "c-new"))
        expectReports(f, [report(.accepted, .peerLeft), report(.declined, .hangup)])
    }

    @Test func winningIncomingTieBreakReportsReplacedOutgoingAndAdoptedIncoming() async {
        let f = CallFixture(sid: { "c-zz" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-aa"))
        await eventually { f.store.state.sid == "c-aa" }
        #expect(f.reports == [report(.proposed, .hangup)])
        #expect(await f.store.declineIncoming())
        expectReports(f, [report(.proposed, .hangup), report(.declined, .hangup)])
    }

    @Test func losingIncomingTieBreakNeverGetsAReport() async {
        let f = CallFixture(sid: { "c-aa" })
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.propose(callAudio), sid: "c-zz"))
        await eventually { f.port.callSends.contains(.reject(callPeer, "c-zz", true)) }
        #expect(f.reports.isEmpty)
        await f.store.hangUp()
        expectReports(f, [report(.proposed, .hangup)])
    }

    @Test func busyRejectedProposalNeverGetsAReport() async {
        let f = CallFixture()
        f.store.handle(f.event(.propose(callAudio)))
        let other = JID(parsing: "carol@waddle.test/phone")!
        f.store.handle(f.event(.propose(callAudio), sid: "busy", from: other))
        await eventually { f.port.callSends.contains(.reject(other, "busy", false)) }
        #expect(f.reports.isEmpty)
        #expect(await f.store.declineIncoming())
        expectReports(f, [report(.declined, .hangup)])
    }

    @Test func groupJoinRollbackIsFailed() async {
        let f = CallFixture()
        f.port.callResults["groupPresence"] = false
        #expect(await !f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice"))
        expectReports(f, [report(.failed, .error, kind: .group)])
    }

    @Test func userCancellingGroupJoinIsProposed() async {
        let f = CallFixture()
        let joining = Task { await f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { f.port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        await f.store.hangUp()
        #expect(await !joining.value)
        expectReports(f, [report(.proposed, .hangup, kind: .group)])
    }

    @Test func mixerEndingPendingGroupJoinIsFailed() async {
        let f = CallFixture()
        let joining = Task { await f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { f.port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        f.store.handle(f.event(.sessionTerminate(reason: .success), from: groupMixer))
        #expect(await !joining.value)
        expectReports(f, [report(.failed, .error, kind: .group)])
    }

    @Test func clearAfterMediaConnectIsAcceptedHangUp() async {
        let media = RecordingCallMedia()
        let f = CallFixture(media: media)
        await connect(f, media: media)
        f.store.clear()
        expectReports(f, [report(.accepted, .hangup)])
    }

    @Test func pushedDeclineReportsOnceBeforeProposalArrives() async {
        let f = CallFixture()
        #expect(f.store.declinePushedCall(sid: "pushed"))
        #expect(f.store.declinePushedCall(sid: "pushed"))
        f.store.handle(f.event(.propose(callAudio), sid: "pushed"))
        await eventually { f.port.callSends.contains(.reject(callPeer, "pushed", false)) }
        expectReports(f, [report(.declined, .hangup)])
    }

    @Test func passiveFinishedMarkersAndGroupPreconditionsNeverReport() async {
        let f = CallFixture()
        f.store.markFinished(sid: "expired-push")
        f.store.dismiss()
        #expect(await !f.store.joinGroupCall(in: groupRoom, media: callAudio, nick: ""))
        f.store.clear()
        #expect(f.reports.isEmpty)
    }

    @Test func ignoredSiblingSignalCannotLeakItsCauseToTimeout() async {
        let f = CallFixture(ringTimeout: .milliseconds(20))
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.reject(reason: nil, tieBreak: false), from: callOwn))
        await eventually { !f.reports.isEmpty }
        expectReports(f, [report(.timeout, .error)])
    }

    private func connect(_ f: CallFixture, media: RecordingCallMedia, sid: String = "c1") async {
        #expect(await f.store.startCall(to: callPeer.bare, media: callAudio))
        f.store.handle(f.event(.sessionAccept(join: callJoin, media: callAudio), sid: sid))
        await eventually { media.calls == [.connect(callJoin, callAudio)] }
        #expect(media.calls == [.connect(callJoin, callAudio)])
    }

    private func report(
        _ setup: CallLifecycleReport.SetupOutcome, _ end: CallLifecycleReport.EndReason,
        duration: CallLifecycleReport.DurationBucket = .none,
        reconnects: CallLifecycleReport.ReconnectBucket = .none, kind: CallLifecycleReport.Kind = .direct
    ) -> CallLifecycleReport {
        CallLifecycleReport(kind: kind, setupOutcome: setup, endReason: end, duration: duration, reconnects: reconnects)
    }

    private func expectReports(_ f: CallFixture, _ reports: [CallLifecycleReport]) {
        #expect(f.reports == reports)
        f.store.dismiss()
        f.store.clear()
        f.store.markFinished(sid: "c1")
        #expect(f.reports == reports)
    }
}
