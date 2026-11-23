import Testing

@testable import WaddleKit

@Suite("Call volume") struct CallVolumeTests {
    @Test func rememberedLevelsStayWithinTheGainRange() {
        #expect(CallVolume.clamp(-0.5) == 0)
        #expect(CallVolume.clamp(0.5) == 0.5)
        #expect(CallVolume.clamp(3) == 2)
        #expect(CallVolume.clamp(.nan) == 1)
        #expect(CallVolume.clamp(.infinity) == 1)
    }

    @Test(arguments: [
        (0.0, 1.0, 0.0),
        (88.0, 1.0, 0.88),
        (99.0, 0.5, 0.99),
        (99.0, 0.98, 1.0),
        (99.0, 1.0, 0.99),
        (100.0, 0.99, 1.0),
        (101.0, 1.0, 1.01),
        (101.0, 1.02, 1.0),
        (101.0, 1.5, 1.01),
        (150.0, 1.0, 1.5),
        (250.0, 1.0, 2.0),
        (Double.nan, 1.0, 1.0),
    ])
    func sliderHasAUnityDetentThatCanBeDraggedThrough(percent: Double, currentGain: Double, expected: Double) {
        #expect(CallVolume.gain(forPercent: percent, currentGain: currentGain) == expected)
    }
}
