import Testing

@testable import WaddleKit

@Suite("Call card summary") struct CallCardSummaryTests {
    @Test func aDirectCallSaysItsMediaAndThatItIsOneToOne() {
        #expect(CallCardSummary.line(status: .connected, kind: .direct(CallMedia(audio: true, video: true)))
            == "Connected · Video call · 1:1")
        #expect(CallCardSummary.line(status: .connecting, kind: .direct(CallMedia(audio: true, video: false)))
            == "Connecting… · Voice call · 1:1")
    }

    @Test func aRoomCallCountsEveryoneAndAlwaysUs() {
        #expect(CallCardSummary.line(status: .connected, kind: .room(count: 4)) == "Connected · Group call · 4 in call")
        #expect(CallCardSummary.line(status: .reconnecting, kind: .room(count: 0)) == "Reconnecting… · Group call · 1 in call")
    }
}
