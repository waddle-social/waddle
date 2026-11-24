import Testing

@testable import WaddleKit

@Suite("Call connection") struct CallConnectionTests {
    @Test func aHealthyConnectionShowsBarsAndSaysNothing() {
        let excellent = CallConnectionChip(quality: .excellent)
        #expect(excellent.bars == 3 && excellent.tone == .neutral && excellent.label == nil)
        let good = CallConnectionChip(quality: .good)
        #expect(good.bars == 2 && good.tone == .neutral && good.label == nil)
        #expect(excellent.isHealthy && good.isHealthy)
    }

    @Test func aPoorConnectionIsAWarningWithOneBar() {
        let chip = CallConnectionChip(quality: .poor)
        #expect(chip.bars == 1 && chip.tone == .warning && chip.label == "Poor connection")
    }

    @Test func aLostConnectionIsDangerWithNoBars() {
        let chip = CallConnectionChip(quality: .lost)
        #expect(chip.bars == 0 && chip.tone == .danger && chip.label == "Connection lost")
    }

    @Test func beforeTheFirstMeasurementItIsQuietAndDoesNotClaimToBeGood() {
        let chip = CallConnectionChip(quality: .unknown)
        #expect(chip.bars == 0 && chip.tone == .neutral && chip.label == nil)
        #expect(chip.status == "Measuring the connection…")
        #expect(!chip.isHealthy)
        #expect(!CallConnectionChip(quality: .poor).isHealthy && !CallConnectionChip(quality: .lost).isHealthy)
    }

    @Test func theStatusNamesEveryState() {
        #expect(CallConnectionChip(quality: .excellent).status == "Excellent connection")
        #expect(CallConnectionChip(quality: .good).status == "Good connection")
        #expect(CallConnectionChip(quality: .poor).status == "Poor connection")
        #expect(CallConnectionChip(quality: .lost).status == "Connection lost")
    }

    // MARK: - After a reconnect

    @Test func aRatingFromBeforeAReconnectIsHeldBack() {
        var gate = CallConnectionRatingGate()
        #expect(gate.shown(.lost) == .lost)
        gate.reconnecting()
        #expect(gate.shown(.lost) == .unknown)
    }

    @Test func aNewRatingIsShownAtOnce() {
        var gate = CallConnectionRatingGate()
        gate.reconnecting()
        gate.rated()
        #expect(gate.shown(.good) == .good)
    }

    /// The SDK reports a rating only when it changes, so a connection as
    /// good after a reconnect as before it would never report again.
    @Test func anUnchangedRatingIsShownAgainOnceTheNewConnectionHasSettled() {
        var gate = CallConnectionRatingGate()
        gate.reconnecting()
        #expect(gate.shown(.excellent) == .unknown)
        gate.settled()
        #expect(gate.shown(.excellent) == .excellent)
    }

    @Test func aSecondDropHoldsTheRatingBackAgain() {
        var gate = CallConnectionRatingGate()
        gate.reconnecting()
        gate.settled()
        gate.reconnecting()
        #expect(gate.shown(.good) == .unknown)
    }
}
