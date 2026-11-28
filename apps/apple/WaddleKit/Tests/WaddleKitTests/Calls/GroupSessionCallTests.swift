import Testing

@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor @Suite("Session group calls") struct GroupSessionCallTests {
    @Test func optedInSessionRoutesRoomCallPresence() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        port.emit(.presence(activePresence))
        port.emit(.authenticationFailed)
        await eventually { coordinator.connection == .authenticationFailed }
        #expect(coordinator.calls.groupPresence.participants == [groupRoom: [
            GroupCallParticipant(nick: "bob", realJID: callPeer, media: CallMedia(audio: true, video: true),
                isHandRaised: true, isMuted: true)]])
        #expect(coordinator.calls.groupPresence.media == [groupRoom: CallMedia(audio: true, video: true)])
        await coordinator.stop()
    }

    @Test func beingRemovedFromARoomForgetsItsCall() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        port.emit(.presence(activePresence))
        await eventually { !coordinator.calls.groupPresence.participants.isEmpty }
        // Our own occupant goes unavailable (XEP-0045 status 110): kicked or left.
        port.emit(.presence(WirePresence(from: JID(parsing: "room@muc.waddle.test/alice")!, kind: .unavailable,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]))))
        await eventually { coordinator.calls.groupPresence.participants.isEmpty }
        #expect(coordinator.calls.groupPresence.participants.isEmpty)
        #expect(coordinator.calls.groupPresence.media.isEmpty)
        await coordinator.stop()
    }

    @Test func aRoomsCallCanOnlyBeJoinedFromInsideTheRoom() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.status.connection = .online
        // The conversation can be open before the room has let us in.
        #expect(!coordinator.canJoinGroupCall(in: groupRoom))
        #expect(await !coordinator.joinGroupCall(in: groupRoom, video: false))
        #expect(port.callSends.isEmpty)
        #expect(coordinator.calls.state == .idle)
    }

    @Test func aRoomsCallIsJoinedUnderTheNickWeHoldThere() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.status.connection = .online
        coordinator.handle(.presence(WirePresence(from: JID(parsing: "room@muc.waddle.test/ali")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]))))
        #expect(coordinator.canJoinGroupCall(in: groupRoom))
        let joining = Task { await coordinator.joinGroupCall(in: groupRoom, video: true) }
        await eventually { !port.callSends.isEmpty }
        #expect(port.callSends == [.groupPresence(groupRoom, "ali", .init(state: .preparing))])
        coordinator.calls.clear()
        _ = await joining.value
    }

    @Test func aRoomsCallCannotBeJoinedWhileAnotherCallIsLive() async {
        let (coordinator, _) = session(handlesCalls: true)
        coordinator.status.connection = .online
        coordinator.handle(.presence(WirePresence(from: JID(parsing: "room@muc.waddle.test/ali")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]))))
        #expect(await coordinator.calls.startCall(to: callPeer.bare, media: callAudio))
        #expect(!coordinator.canJoinGroupCall(in: groupRoom))
        coordinator.calls.clear()
    }

    @Test func joiningAnAdvertisedCallUsesTheExistingRoomOccupantAndLeavesTheCameraOff() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.status.connection = .online
        coordinator.handle(.presence(WirePresence(from: JID(parsing: "room@muc.waddle.test/ali")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]))))
        coordinator.handle(.presence(activePresence))

        let joining = Task { await coordinator.joinLiveRoomCall(in: groupRoom) }
        await eventually { !port.callSends.isEmpty }
        #expect(port.callSends == [.groupPresence(groupRoom, "ali", .init(state: .preparing))])
        guard case .joiningGroup(let attempt) = coordinator.calls.state else {
            Issue.record("Expected to be joining the advertised call")
            coordinator.calls.clear()
            _ = await joining.value
            return
        }
        #expect(attempt.media == CallMedia(audio: true, video: false))
        coordinator.calls.clear()
        _ = await joining.value
    }

    @Test func joiningFromAStaleRowDoesNotStartANewCall() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.status.connection = .online
        coordinator.handle(.presence(WirePresence(from: JID(parsing: "room@muc.waddle.test/ali")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]))))
        coordinator.handle(.presence(activePresence))
        #expect(coordinator.calls.groupPresence.hasCall(in: groupRoom))
        coordinator.handle(.presence(WirePresence(from: activePresence.from, kind: .unavailable)))

        await coordinator.joinLiveRoomCall(in: groupRoom)

        #expect(port.callSends.isEmpty)
        #expect(coordinator.calls.state == .idle)
    }

    @Test func optedOutSessionIgnoresRoomCallPresence() async {
        let (coordinator, port) = session(handlesCalls: false)
        coordinator.start()
        port.emit(.presence(activePresence))
        port.emit(.authenticationFailed)
        await eventually { coordinator.connection == .authenticationFailed }
        #expect(coordinator.calls.groupPresence.participants.isEmpty)
        #expect(coordinator.calls.groupPresence.media.isEmpty)
        await coordinator.stop()
    }

    @Test func streamLossClearsGroupPresenceWhilePreservingTheLiveSlot() async throws {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        let joining = Task { await coordinator.calls.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        let sid = try #require(coordinator.calls.state.sid)
        port.emit(.presence(WirePresence(from: JID(parsing: "room@muc.waddle.test/alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]),
            groupCall: .init(isPreparing: true, media: nil))))
        await eventually { port.callSends.contains(.groupInitiate(groupRoom, callOwn, sid, false)) }
        port.emit(.call(CallEvent(from: JID(parsing: "calls.waddle.test/mixer")!, sid: sid,
            kind: .sessionAccept(join: groupJoin, media: callAudio))))
        #expect(await joining.value)
        port.emit(.presence(activePresence))
        port.emit(.disconnected)
        port.emit(.authenticationFailed)
        await eventually { coordinator.connection == .authenticationFailed }
        #expect(coordinator.calls.groupPresence.participants.isEmpty)
        #expect(coordinator.calls.groupPresence.media.isEmpty)
        #expect(coordinator.calls.state == .inGroup(GroupCall(room: groupRoom, sid: sid, media: callAudio,
            join: groupJoin, nick: "alice")))
        await coordinator.stop()
    }

    /// The call's media goes on while the stream is replaced, but the room we come back into has forgotten we are in its call.
    @Test func comingBackIntoTheRoomOfOurCallSaysWeAreStillInIt() async throws {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        let joining = Task { await coordinator.calls.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        let sid = try #require(coordinator.calls.state.sid)
        port.emit(.presence(ownPresence(groupCall: .init(isPreparing: true, media: nil))))
        await eventually { port.callSends.contains(.groupInitiate(groupRoom, callOwn, sid, false)) }
        port.emit(.call(CallEvent(from: JID(parsing: "calls.waddle.test/mixer")!, sid: sid,
            kind: .sessionAccept(join: groupJoin, media: callAudio))))
        #expect(await joining.value)
        #expect(await coordinator.calls.setHandRaised(true))

        port.emit(.disconnected)
        await eventually { coordinator.calls.groupPresence.participants.isEmpty }
        port.callSends = []
        // The join of the new stream: our own presence, with no call in it.
        port.emit(.presence(ownPresence(groupCall: nil)))
        await eventually { !port.callSends.isEmpty }
        #expect(port.callSends == [
            .groupPresence(groupRoom, "alice", .init(state: .active(video: false), isHandRaised: true)),
        ])
        await coordinator.stop()
    }

    @Test func joiningARoomWeAreInNoCallOfSaysNothingAboutCalls() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        port.emit(.presence(ownPresence(groupCall: nil)))
        await eventually { coordinator.presence.joinedRooms.contains(groupRoom) }
        for _ in 0..<5 { await Task.yield() }
        #expect(port.callSends.isEmpty)
        await coordinator.stop()
    }

    private func ownPresence(groupCall: GroupCallMarker?) -> WirePresence {
        WirePresence(from: JID(parsing: "room@muc.waddle.test/alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callOwn, statusCodes: [110]), groupCall: groupCall)
    }

    /// A reaction is for the call's stage: it is no row of the room, no unread and no alert.
    @Test func aReactionInOurRoomsCallGoesToTheStageAndNowhereElse() async throws {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        let joining = Task { await coordinator.calls.joinGroupCall(in: groupRoom, media: callAudio, nick: "alice") }
        await eventually { port.callSends.contains(.groupPresence(groupRoom, "alice", .init(state: .preparing))) }
        let sid = try #require(coordinator.calls.state.sid)
        port.emit(.presence(ownPresence(groupCall: .init(isPreparing: true, media: nil))))
        await eventually { port.callSends.contains(.groupInitiate(groupRoom, callOwn, sid, false)) }
        port.emit(.call(CallEvent(from: JID(parsing: "calls.waddle.test/mixer")!, sid: sid,
            kind: .sessionAccept(join: groupJoin, media: callAudio))))
        #expect(await joining.value)

        func message(body: String?, id: String) -> WireMessage {
            var message = WireMessage(
                type: .groupchat, from: JID(parsing: "room@muc.waddle.test/bob")!, to: callOwn,
                identity: MessageIdentity(messageID: id, stanzaIDs: [StanzaID(id: "s-\(id)", by: groupRoom)]), body: body
            )
            message.inCallReaction = InCallReaction(sid: "bobs-own", emoji: "🎉")
            return message
        }
        port.emit(.message(message(body: nil, id: "r1")))
        await eventually { coordinator.calls.reactions.bursts.count == 1 }
        let timeline = coordinator.timelines.timeline(for: .room(groupRoom))
        #expect(timeline.items.isEmpty)
        #expect(coordinator.unread.count(for: .room(groupRoom)) == 0)

        // A message with words in it is a message, whatever else it carries.
        port.emit(.message(message(body: "hello", id: "m1")))
        await eventually { !timeline.items.isEmpty }
        #expect(timeline.items.map(\.body) == ["hello"])
        #expect(coordinator.calls.reactions.bursts.count == 1)
        await coordinator.stop()
    }

    @Test func stopForgetsRoomCallPresence() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        port.emit(.presence(activePresence))
        await eventually { !coordinator.calls.groupPresence.participants.isEmpty }
        #expect(coordinator.calls.groupPresence.media == [groupRoom: CallMedia(audio: true, video: true)])
        await coordinator.stop()
        #expect(coordinator.calls.groupPresence.participants.isEmpty)
        #expect(coordinator.calls.groupPresence.media.isEmpty)
    }

    @Test func stoppedSessionIgnoresRoomCallPresenceStillArrivingOnTheStream() async {
        let (coordinator, port) = session(handlesCalls: true)
        var authenticationFailures = 0
        coordinator.onAuthenticationFailed = { authenticationFailures += 1 }
        coordinator.start()
        port.emit(.authenticationFailed)
        port.emit(.presence(activePresence))
        port.emit(.authenticationFailed)
        await eventually { authenticationFailures == 2 }
        #expect(coordinator.calls.groupPresence.participants.isEmpty)
        #expect(coordinator.calls.groupPresence.media.isEmpty)
        await coordinator.stop()
    }

    @Test func clearingCallStoreForgetsRoomCallPresence() async {
        let (coordinator, port) = session(handlesCalls: true)
        coordinator.start()
        port.emit(.presence(activePresence))
        await eventually { !coordinator.calls.groupPresence.participants.isEmpty }
        coordinator.calls.clear()
        #expect(coordinator.calls.groupPresence.participants.isEmpty)
        #expect(coordinator.calls.groupPresence.media.isEmpty)
        #expect(coordinator.calls.state == .idle)
        await coordinator.stop()
    }

    private var activePresence: WirePresence {
        // Someone else's: our own device is in a call only while our slot says so.
        WirePresence(from: JID(parsing: "room@muc.waddle.test/bob")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: callPeer, statusCodes: []),
            groupCall: .init(isPreparing: false, media: CallMedia(audio: true, video: true), isHandRaised: true, isMuted: true))
    }

    private func session(handlesCalls: Bool) -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        return (SessionCoordinator(account: AccountIdentity(jid: callOwn.bare, nick: "alice", resource: callOwn.resource),
            port: port, handlesCalls: handlesCalls), port)
    }
}
