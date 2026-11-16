import Testing

@testable import WaddleKit

@Suite("Group call stage") struct GroupCallStageTests {
    private let room = BareJID(parsing: "room@muc.waddle.test")!

    private var inGroup: CallState {
        .inGroup(GroupCall(room: room, sid: "g1", media: callVideo, join: callJoin, nick: "alice"))
    }

    @Test(arguments: [CallMediaPhase.idle, .connecting, .connected, .reconnecting])
    func aJoinAttemptIsJoiningWhateverMediaIsDoing(phase: CallMediaPhase) {
        let state = CallState.joiningGroup(GroupCallAttempt(room: room, sid: "g1", media: callAudio, nick: "alice"))
        #expect(GroupCallStage(state: state, media: phase) == .joining(room: room, media: callAudio))
    }

    @Test(arguments: [CallMediaPhase.idle, .connecting])
    func anAcceptedSessionWhoseMediaIsNotUpIsConnecting(phase: CallMediaPhase) {
        #expect(GroupCallStage(state: inGroup, media: phase) == .connecting(room: room, media: callVideo))
    }

    @Test func anAcceptedSessionWithConnectedMediaIsInCall() {
        #expect(GroupCallStage(state: inGroup, media: .connected) == .inCall(room: room, media: callVideo))
    }

    @Test func anAcceptedSessionWhoseMediaIsReconnectingIsReconnecting() {
        #expect(GroupCallStage(state: inGroup, media: .reconnecting) == .reconnecting(room: room, media: callVideo))
    }

    @Test(arguments: [
        CallState.idle,
        .incoming(IncomingCall(from: callPeer, sid: "c1", media: callAudio)),
        .outgoing(OutgoingCall(to: callPeer.bare, sid: "c1", media: callAudio)),
        .active(ActiveCall(peer: callPeer, sid: "c1", media: callAudio, join: callJoin)),
        .ended(sid: "g1", reason: .finished(nil)),
    ])
    func aSlotWithoutAGroupPhaseHasNoGroupStage(state: CallState) {
        #expect(GroupCallStage(state: state, media: .connected) == nil)
    }

    @Test func everyStageNamesItsRoomAndMedia() {
        let stage = GroupCallStage.reconnecting(room: room, media: callVideo)
        #expect(stage.room == room)
        #expect(stage.media == callVideo)
    }

    @Test(arguments: [GroupCallStage.inCall(room: BareJID(parsing: "room@muc.waddle.test")!, media: callAudio),
        .reconnecting(room: BareJID(parsing: "room@muc.waddle.test")!, media: callAudio)])
    func theRoomsMediaIsUpInCallAndWhileReconnecting(stage: GroupCallStage) {
        #expect(stage.hasMedia)
    }

    @Test(arguments: [GroupCallStage.joining(room: BareJID(parsing: "room@muc.waddle.test")!, media: callAudio),
        .connecting(room: BareJID(parsing: "room@muc.waddle.test")!, media: callAudio)])
    func theRoomsMediaIsNotUpWhileJoiningOrConnecting(stage: GroupCallStage) {
        #expect(!stage.hasMedia)
    }
}
