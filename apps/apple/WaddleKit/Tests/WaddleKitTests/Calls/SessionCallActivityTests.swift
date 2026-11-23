import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Session call activity") struct SessionCallActivityTests {
    private let audio = UserActivity(general: "talking", specific: "on_the_phone")
    private let video = UserActivity(general: "talking", specific: "on_video_phone")
    private let prior = UserActivity(general: "working", specific: "coding", text: "Shipping")

    @Test(arguments: [false, true]) func aDirectCallPublishesItsMediaAndRetractsOnHangup(hasVideo: Bool) async {
        let (session, port) = makeSession()
        await establishDirectCall(in: session, port: port, media: hasVideo ? callVideo : callAudio)
        let mark = hasVideo ? video : audio
        await eventually { session.status.activity == mark }
        #expect(port.publishedActivities == [mark])
        #expect(session.status.activity == mark)

        await session.calls.hangUp()
        await eventually { port.publishedActivities.count == 2 }
        #expect(port.publishedActivities == [mark, nil])
        #expect(session.status.activity == nil)
        await session.stop()
    }

    @Test func aGroupCallPublishesItsMediaAndRestoresThePriorActivity() async throws {
        let (session, port) = makeSession()
        port.ownActivity = .success(prior)
        let occupantJID = groupRoom.with(resource: "alice")!
        let occupant = WirePresence.Occupant(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110])
        port.emit(.presence(WirePresence(from: occupantJID, kind: .available, occupant: occupant)))
        await eventually { session.canJoinGroupCall(in: groupRoom) }
        let joining = Task { await session.joinGroupCall(in: groupRoom, video: true) }
        await eventually { port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        let sid = try #require(session.calls.state.sid)
        #expect(port.publishedActivities.isEmpty)
        let echo = WirePresence(
            from: occupantJID, kind: .available, occupant: occupant, groupCall: .init(isPreparing: true, media: nil))
        port.emit(.presence(echo))
        await eventually { port.callSends.contains(.groupInitiate(groupRoom, callOwn, sid, true)) }
        port.emit(.call(.init(from: groupMixer, sid: sid, kind: .sessionAccept(join: groupJoin, media: callVideo))))
        #expect(await joining.value)
        await eventually { session.status.activity == video }
        #expect(port.publishedActivities == [video])
        #expect(session.status.activity == video)

        await session.calls.hangUp()
        await eventually { session.status.activity == prior }
        #expect(port.publishedActivities == [video, prior])
        #expect(session.status.activity == prior)
        await session.stop()
    }

    @Test func anotherDevicesCallMarkSurvivesOurCall() async {
        let (session, port) = makeSession()
        port.ownActivity = .success(video)
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { session.status.activity == video }
        #expect(session.status.activity == video)
        #expect(port.publishedActivities.isEmpty)

        await session.calls.hangUp()
        await session.stop()
        #expect(port.publishedActivities.isEmpty)
        #expect((try? port.ownActivity.get()) == video)
    }

    @Test func aFailedLookupPreventsBothPublishingAndCleanup() async {
        let (session, port) = makeSession()
        port.ownActivity = .failure(.timeout)
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { port.ownActivityLookupCount == 1 }
        #expect(port.ownActivityLookupCount == 1)
        #expect(port.publishedActivities.isEmpty)

        await session.calls.hangUp()
        await session.stop()
        #expect(port.ownActivityLookupCount == 1)
        #expect(port.publishedActivities.isEmpty)
    }

    @Test(arguments: [false, true]) func signOutWaitsForCleanupBeforeClosingTheStream(hasPrior: Bool) async {
        let (session, port) = makeSession()
        let previous = hasPrior ? prior : nil
        port.ownActivity = .success(previous)
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { session.status.activity == audio }
        port.activityWrites.isHolding = true

        let signingOut = Task { await session.signOut() }
        await eventually { port.activityWrites.count == 1 }
        #expect(port.publishedActivities == [audio, previous])
        #expect(port.disconnectCount == 0)
        port.activityWrites.isHolding = false
        port.activityWrites.release()
        await signingOut.value

        #expect(port.disconnectCount == 1)
        #expect(session.status.activity == previous)
    }

    @Test(arguments: [false, true]) func aReplacementDuringTheCallIsPreserved(isCallMark: Bool) async {
        let (session, port) = makeSession()
        port.ownActivity = .success(prior)
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { session.status.activity == audio }
        let replacement = isCallMark ? video : UserActivity(general: "relaxing", specific: "reading")
        port.ownActivity = .success(replacement)

        await session.calls.hangUp()
        await eventually { session.status.activity == replacement }
        #expect(port.publishedActivities == [audio])
        #expect(session.status.activity == replacement)
        await session.stop()
    }

    @Test func aCallThatEndsDuringTheLookupDoesNotPublish() async {
        let (session, port) = makeSession()
        port.ownActivityFetches.isHolding = true
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { port.ownActivityFetches.count == 1 }
        #expect(port.ownActivityFetches.count == 1)

        await session.calls.hangUp()
        port.ownActivityFetches.isHolding = false
        port.ownActivityFetches.release()
        await session.stop()
        #expect(port.publishedActivities.isEmpty)
    }

    @Test func aCallRestoresAManualWriteAlreadyInProgress() async throws {
        let (session, port) = makeSession()
        port.activityWrites.isHolding = true
        let edit = Task { try await session.setActivity(prior) }
        await eventually { port.activityWrites.count == 1 }
        #expect(port.publishedActivities == [prior])

        await establishDirectCall(in: session, port: port, media: callAudio, earlierWrites: [prior])
        #expect(port.publishedActivities == [prior])

        port.activityWrites.isHolding = false
        port.activityWrites.release()
        try await edit.value
        await eventually { session.status.activity == audio }
        await session.calls.hangUp()
        await eventually { port.publishedActivities.count == 3 }
        #expect(port.publishedActivities == [prior, audio, prior])
        #expect(session.status.activity == prior)
        await session.stop()
    }

    @Test func signOutWaitsForAnInFlightCallMarkAndRetractsIt() async {
        let (session, port) = makeSession()
        port.activityWrites.isHolding = true
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { port.activityWrites.count == 1 }
        #expect(port.publishedActivities == [audio])

        let signingOut = Task { await session.signOut() }
        await eventually { session.isStopped }
        #expect(port.disconnectCount == 0)
        port.activityWrites.release()
        // Hold the cleanup separately to verify the stream stays up for it too.
        await eventually { port.publishedActivities.count == 2 }
        #expect(port.publishedActivities == [audio, nil])
        #expect(port.disconnectCount == 0)
        port.activityWrites.isHolding = false
        port.activityWrites.release()
        await signingOut.value
        #expect(port.disconnectCount == 1)
        #expect(session.status.activity == nil)
    }

    @Test func aSecondCallWaitsForTheFirstCallsCleanup() async {
        let (session, port) = makeSession()
        await establishDirectCall(in: session, port: port, media: callAudio)
        await eventually { session.status.activity == audio }

        port.ownActivity = .success(prior)
        port.ownActivityFetches.isHolding = true
        await session.calls.hangUp()
        await eventually { port.ownActivityFetches.count == 1 }
        #expect(port.ownActivityFetches.count == 1)

        await establishDirectCall(in: session, port: port, media: callAudio, sid: "c-next", earlierWrites: [audio])
        port.ownActivityFetches.isHolding = false
        port.ownActivityFetches.release()
        await eventually { port.publishedActivities.count == 2 }
        #expect(port.publishedActivities == [audio, audio])

        await session.calls.hangUp()
        await session.stop()
        #expect(port.publishedActivities == [audio, audio, prior])
        #expect(session.status.activity == prior)
    }

    private func makeSession() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let account = AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource)
        let session = SessionCoordinator(account: account, port: port, handlesCalls: true)
        session.start()
        port.emit(.connected)
        return (session, port)
    }

    private func establishDirectCall(
        in session: SessionCoordinator, port: FakePort, media: CallMedia, sid: String = "c-activity",
        earlierWrites: [UserActivity?] = []
    ) async {
        port.emit(.call(.init(from: callPeer, sid: sid, kind: .propose(media))))
        await eventually { session.calls.state.sid == sid }
        #expect(port.publishedActivities == earlierWrites)
        #expect(await session.calls.acceptIncoming())
        port.emit(.call(.init(from: callPeer, sid: sid, kind: .sessionInitiate(join: callJoin, media: media))))
        await eventually { session.calls.state.isEstablished }
        #expect(session.calls.state.isEstablished)
    }
}
