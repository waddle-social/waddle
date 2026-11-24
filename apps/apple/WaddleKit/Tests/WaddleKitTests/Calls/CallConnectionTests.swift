import Testing

@testable import WaddleKit

@Suite("Call connection") struct CallConnectionTests {
    @Test func aHealthyConnectionShowsBarsAndSaysNothing() {
        let excellent = CallConnectionChip(quality: .excellent, isReconnecting: false)
        #expect(excellent.bars == 3 && excellent.tone == .neutral && excellent.label == nil)
        let good = CallConnectionChip(quality: .good, isReconnecting: false)
        #expect(good.bars == 2 && good.tone == .neutral && good.label == nil)
    }

    @Test func aPoorConnectionIsAWarningWithOneBar() {
        let chip = CallConnectionChip(quality: .poor, isReconnecting: false)
        #expect(chip == CallConnectionChip(quality: .poor, isReconnecting: false))
        #expect(chip.bars == 1 && chip.tone == .warning && chip.label == "Poor connection")
    }

    @Test func aLostConnectionIsDangerWithNoBars() {
        let chip = CallConnectionChip(quality: .lost, isReconnecting: false)
        #expect(chip.bars == 0 && chip.tone == .danger && chip.label == "Connection lost")
    }

    @Test(arguments: [CallConnectionQuality.excellent, .good, .poor, .lost, .unknown])
    func reconnectingOutranksWhateverWasMeasured(quality: CallConnectionQuality) {
        let chip = CallConnectionChip(quality: quality, isReconnecting: true)
        #expect(chip.bars == 0 && chip.tone == .danger && chip.label == "Reconnecting…")
    }

    @Test func beforeTheFirstMeasurementItIsQuietAndDoesNotClaimToBeGood() {
        let chip = CallConnectionChip(quality: .unknown, isReconnecting: false)
        #expect(chip.bars == 0 && chip.tone == .neutral && chip.label == nil)
        #expect(chip.status == "Measuring the connection…")
    }

    @Test func theStatusNamesEveryState() {
        #expect(CallConnectionChip(quality: .excellent, isReconnecting: false).status == "Excellent connection")
        #expect(CallConnectionChip(quality: .good, isReconnecting: false).status == "Good connection")
        #expect(CallConnectionChip(quality: .poor, isReconnecting: false).status == "Poor connection")
        #expect(CallConnectionChip(quality: .lost, isReconnecting: false).status == "Connection lost")
    }
}
