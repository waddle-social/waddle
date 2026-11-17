import Testing
import WaddleKit

@MainActor @Suite("Speaking hold") struct SpeakingHoldTests {
    private let alice = BareJID(parsing: "alice@waddle.test")!
    private let bob = BareJID(parsing: "bob@waddle.test")!

    @Test func nobodyIsShownSpeakingAtFirst() {
        #expect(SpeakingHold().shown == [])
    }

    @Test func someoneWhoStartsSpeakingShowsAtOnce() {
        let hold = SpeakingHold(hold: .seconds(60))
        hold.update([alice])
        #expect(hold.shown == [alice])
    }

    @Test func someoneWhoStopsSpeakingStaysShownDuringTheHold() {
        let hold = SpeakingHold(hold: .seconds(60))
        hold.update([alice])
        hold.update([])
        #expect(hold.shown == [alice])
    }

    @Test func someoneWhoStopsSpeakingIsReleasedAfterTheHold() async {
        let hold = SpeakingHold(hold: .milliseconds(20))
        hold.update([alice])
        hold.update([])
        await eventually { hold.shown.isEmpty }
        #expect(hold.shown == [])
    }

    @Test func someoneWhoSpeaksAgainWithinTheHoldIsKept() async throws {
        let hold = SpeakingHold(hold: .milliseconds(50))
        hold.update([alice])
        hold.update([])
        hold.update([alice])
        try await Task.sleep(for: .milliseconds(200))
        #expect(hold.shown == [alice])
    }

    @Test func eachPersonIsHeldOnTheirOwn() async {
        let hold = SpeakingHold(hold: .milliseconds(20))
        hold.update([alice, bob])
        hold.update([bob])
        await eventually { hold.shown == [bob] }
        #expect(hold.shown == [bob])
    }

    @Test func theDefaultHoldIsThreeHundredMilliseconds() {
        #expect(SpeakingHold.defaultHold == .milliseconds(300))
    }
}
