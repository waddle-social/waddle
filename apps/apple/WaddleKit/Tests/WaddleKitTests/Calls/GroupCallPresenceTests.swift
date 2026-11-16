import Testing
import WaddleKit

@MainActor @Suite("Group call presence") struct GroupCallPresenceTests {
    private let room = BareJID(parsing: "room@muc.waddle.test")!
    private let own = JID(parsing: "alice@waddle.test/phone")!
    private let peer = JID(parsing: "alice@waddle.test/tablet")!

    @Test func activePresenceAddsTheNickWithOwnerAndMedia() {
        let presence = GroupCallPresence()
        presence.apply(WirePresence(
            from: JID(parsing: "room@muc.waddle.test/alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: own, statusCodes: []),
            groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: false),
                isHandRaised: false, isMuted: false)))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: own,
            media: CallMedia(audio: true, video: false), isHandRaised: false, isMuted: false)]])
        #expect(presence.media == [room: CallMedia(audio: true, video: false)])
    }

    @Test func presenceWithoutMujiRemovesTheNick() {
        let presence = GroupCallPresence()
        presence.apply(active("alice"))
        presence.apply(WirePresence(from: JID(parsing: "room@muc.waddle.test/alice")!, kind: .available))
        #expect(presence.participants.isEmpty)
        #expect(presence.media.isEmpty)
    }

    @Test func preparingEchoRegisteredBeforeSendCannotBeMissed() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        presence.apply(WirePresence(from: JID(parsing: "room@muc.waddle.test/alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: own, statusCodes: []),
            groupCall: GroupCallMarker(isPreparing: true, media: nil)))
        #expect(await presence.awaitPreparingEcho(waiter, timeout: .milliseconds(50)))
        #expect(presence.participants.isEmpty)
    }

    @Test func twoConsumersOfOnePreparingHandleBothReceiveItsEcho() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        var started = 0
        var completions: [Bool] = []
        for _ in 0..<2 {
            Task {
                started += 1
                completions.append(await presence.awaitPreparingEcho(waiter, timeout: .seconds(5)))
            }
        }
        await eventually { started == 2 }
        presence.apply(preparing("alice", realJID: own))
        await eventually { completions.count == 2 }
        #expect(completions == [true, true])
        presence.clear()
    }

    @Test func twoConsumersOfOnePreparingHandleBothFinishOnTimeout() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        var completions: [Bool] = []
        for _ in 0..<2 {
            Task {
                completions.append(await presence.awaitPreparingEcho(waiter, timeout: .milliseconds(10)))
            }
        }
        await eventually { completions.count == 2 }
        #expect(completions == [false, false])
        presence.clear()
    }

    @Test func unavailablePresenceRemovesEvenWithActiveContents() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own))
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .unavailable,
            occupant: .init(affiliation: nil, role: nil, realJID: own, statusCodes: []),
            groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: true))))
        #expect(presence.participants.isEmpty)
        #expect(presence.media.isEmpty)
    }

    @Test func freshRoomRejoinNeedsRenewedActiveContents() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own, video: true))
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .unavailable,
            occupant: .init(affiliation: nil, role: nil, realJID: own, statusCodes: [])))
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: own, statusCodes: [])))
        presence.apply(preparing("alice", realJID: own))
        #expect(presence.participants.isEmpty)
        #expect(presence.media.isEmpty)
        presence.apply(active("alice", realJID: own, video: true))
        presence.apply(active("alice", realJID: own, video: true))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: own,
            media: CallMedia(audio: true, video: true))]])
        #expect(presence.media == [room: CallMedia(audio: true, video: true)])
    }

    @Test func preparingNeverAddsOrClearsActiveMembership() {
        let presence = GroupCallPresence()
        presence.apply(preparing("alice", realJID: own))
        #expect(presence.participants.isEmpty)
        presence.apply(active("alice", realJID: own))
        presence.apply(preparing("alice", realJID: peer))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: own,
            media: CallMedia(audio: true, video: false))]])
    }

    @Test func duplicateAndUnknownPresencesAreNoOpsAndOrderIsStable() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own))
        presence.apply(active("bob", realJID: peer))
        presence.apply(active("alice", realJID: own))
        presence.apply(WirePresence(from: room.with(resource: "ghost")!, kind: .available))
        #expect(presence.participants == [room: [
            GroupCallParticipant(nick: "alice", realJID: own, media: CallMedia(audio: true, video: false)),
            GroupCallParticipant(nick: "bob", realJID: peer, media: CallMedia(audio: true, video: false))]])
    }

    @Test func mediaAggregatesAcrossParticipantsAndShrinksOnLeave() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own))
        presence.apply(active("bob", realJID: peer, video: true))
        #expect(presence.media == [room: CallMedia(audio: true, video: true)])
        presence.apply(WirePresence(from: room.with(resource: "bob")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: peer, statusCodes: [])))
        #expect(presence.media == [room: CallMedia(audio: true, video: false)])
        #expect(presence.participants[room]?.map(\.nick) == ["alice"])
    }

    @Test func sameNickResourcesKeepSeparateMembershipAndLastOwner() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own, video: true))
        presence.apply(active("alice", realJID: peer))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: peer,
            media: CallMedia(audio: true, video: true))]])
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: own, statusCodes: [])))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: peer,
            media: CallMedia(audio: true, video: false))]])
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: peer, statusCodes: [])))
        #expect(presence.participants.isEmpty)
    }

    @Test func identitylessLeaveRemovesAllSameNickResources() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own))
        presence.apply(active("alice", realJID: peer))
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .available))
        #expect(presence.participants.isEmpty)
    }

    @Test func localClearRemovesExactIdentityAndAggregateButKeepsSibling() {
        let presence = GroupCallPresence()
        presence.apply(active("alice"))
        presence.apply(active("alice", realJID: own))
        presence.apply(active("alice", realJID: peer))
        presence.clearParticipant(in: room, nick: "alice", realJID: own)
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: peer,
            media: CallMedia(audio: true, video: false))]])
        presence.clearParticipant(in: room, nick: "alice", realJID: peer)
        #expect(presence.participants.isEmpty)
        #expect(presence.media.isEmpty)
    }

    @Test func raisedHandAndMuteTrackFlagsAndClearOnLeave() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", raised: true, muted: true))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: nil,
            media: CallMedia(audio: true, video: false), isHandRaised: true, isMuted: true)]])
        presence.apply(active("alice", raised: false, muted: true))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: nil,
            media: CallMedia(audio: true, video: false), isHandRaised: false, isMuted: true)]])
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .available))
        #expect(presence.participants.isEmpty)
    }

    @Test func preparingEchoTimesOutAndLateEchoCannotReviveIt() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        #expect(await !presence.awaitPreparingEcho(waiter, timeout: .milliseconds(10)))
        presence.apply(preparing("alice", realJID: own))
        #expect(await !presence.awaitPreparingEcho(waiter, timeout: .milliseconds(10)))
    }

    @Test func siblingResourceEchoCannotSatisfyIdentityKeyedWaiter() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        presence.apply(preparing("alice", realJID: peer))
        #expect(await !presence.awaitPreparingEcho(waiter, timeout: .milliseconds(10)))
    }

    @Test func cancellationFailsPendingEchoWait() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        presence.cancelPreparationWaiters(in: room, nick: "alice")
        #expect(await !presence.awaitPreparingEcho(waiter, timeout: .seconds(1)))
    }

    @Test func replacingEchoCancelsOnlyThePreviousWaiter() async {
        let presence = GroupCallPresence()
        let old = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        let replacement = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        #expect(await !presence.awaitPreparingEcho(old, timeout: .seconds(1)))
        presence.apply(preparing("alice", realJID: own))
        #expect(await presence.awaitPreparingEcho(replacement, timeout: .seconds(1)))
    }

    @Test func noOtherPreparingCompletesImmediatelyForEmptyRoomAndOwnEntry() async {
        let presence = GroupCallPresence()
        #expect(await presence.awaitNoOtherPreparing(in: room, nick: "alice", realJID: own, timeout: .seconds(1)))
        presence.apply(preparing("alice", realJID: own))
        #expect(await presence.awaitNoOtherPreparing(in: room, nick: "alice", realJID: own, timeout: .seconds(1)))
    }

    @Test func noOtherPreparingWaitsForOtherOccupantToDeclareContents() async {
        let presence = GroupCallPresence()
        presence.apply(preparing("bob", realJID: peer))
        var started = false
        let waiting = Task {
            started = true
            return await presence.awaitNoOtherPreparing(in: room, nick: "alice", realJID: own, timeout: .seconds(1))
        }
        await eventually { started }
        presence.apply(active("bob", realJID: peer, video: true))
        #expect(await waiting.value)
        #expect(presence.media == [room: CallMedia(audio: true, video: true)])
    }

    @Test func noOtherPreparingTimesOutForSiblingOrUnknownIdentity() async {
        let presence = GroupCallPresence()
        presence.apply(preparing("alice", realJID: peer))
        #expect(await !presence.awaitNoOtherPreparing(in: room, nick: "alice", realJID: own, timeout: .milliseconds(10)))
        presence.clear()
        presence.apply(preparing("alice"))
        #expect(await !presence.awaitNoOtherPreparing(in: room, nick: "alice", realJID: own, timeout: .milliseconds(10)))
    }

    @Test func cancellingCallerFinishesItsWaitWithoutLeakingContinuation() async {
        let presence = GroupCallPresence()
        let waiter = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        var started = false
        let waiting = Task {
            started = true
            return await presence.awaitPreparingEcho(waiter, timeout: .seconds(60))
        }
        await eventually { started }
        waiting.cancel()
        #expect(await !waiting.value)
        presence.apply(preparing("alice", realJID: own))
        #expect(await !presence.awaitPreparingEcho(waiter, timeout: .seconds(1)))
    }

    @Test func clearCancelsBothKindsOfPreparationWaitersAndForgetsMembership() async {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own, raised: true, muted: true))
        presence.apply(preparing("bob", realJID: peer))
        let echo = presence.registerPreparingEcho(in: room, nick: "alice", realJID: own)
        var started = false
        let waiting = Task {
            started = true
            return await presence.awaitNoOtherPreparing(in: room, nick: "alice", realJID: own, timeout: .seconds(60))
        }
        await eventually { started }
        presence.clear()
        #expect(await !waiting.value)
        #expect(await !presence.awaitPreparingEcho(echo, timeout: .seconds(1)))
        #expect(presence.participants.isEmpty)
        #expect(presence.media.isEmpty)
    }

    @Test func bareSendersAndNonAvailabilityPresenceCannotChangeMembership() {
        let presence = GroupCallPresence()
        presence.apply(active("alice", realJID: own))
        presence.apply(WirePresence(from: JID(bare: room, resource: nil)!, kind: .available,
            groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: true))))
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .error(condition: "forbidden")))
        presence.apply(WirePresence(from: room.with(resource: "alice")!, kind: .other("subscribe")))
        #expect(presence.participants == [room: [GroupCallParticipant(nick: "alice", realJID: own,
            media: CallMedia(audio: true, video: false))]])
    }

    private func preparing(_ nick: String, realJID: JID? = nil) -> WirePresence {
        WirePresence(from: room.with(resource: nick)!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: realJID, statusCodes: []),
            groupCall: GroupCallMarker(isPreparing: true, media: nil))
    }

    private func active(_ nick: String, realJID: JID? = nil, video: Bool = false,
                        raised: Bool = false, muted: Bool = false) -> WirePresence {
        WirePresence(from: room.with(resource: nick)!, kind: .available,
            occupant: .init(affiliation: nil, role: nil, realJID: realJID, statusCodes: []),
            groupCall: GroupCallMarker(isPreparing: false, media: CallMedia(audio: true, video: video),
                isHandRaised: raised, isMuted: muted))
    }
}
