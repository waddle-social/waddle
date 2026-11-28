import Testing

@testable import WaddleKit

@Suite("Starting a call device") @MainActor struct CallCaptureAttemptTests {
    @Test func checkingAgainLooksForANewlyConnectedDevice() async {
        var connected = false
        var starts = 0
        func check() async -> CallCaptureAttempt.Outcome {
            await CallCaptureAttempt.run(
                isCurrent: { true }, requestAccess: { true }, deviceAvailability: { connected },
                start: { starts += 1; return nil }
            )
        }

        let withoutDevice = await check()
        #expect(withoutDevice == .failed(.missing))
        #expect(starts == 0)
        connected = true
        let withDevice = await check()
        #expect(withDevice == .started)
        #expect(starts == 1)
    }

    @Test func aRefusalIsReportedEvenWhenTheDeviceListCouldBeEmpty() async {
        let outcome = await CallCaptureAttempt.run(
            isCurrent: { true }, requestAccess: { false },
            deviceAvailability: { Issue.record("A refused device doesn't need to be listed"); return false },
            start: { Issue.record("A refused device can't start"); return nil }
        )
        #expect(outcome == .failed(.denied))
    }

    @Test(arguments: [CallMediaIssue.Reason.inUse, .failed])
    func anUnknownDeviceListStillLetsCaptureTry(failure: CallMediaIssue.Reason) async {
        let outcome = await CallCaptureAttempt.run(
            isCurrent: { true }, requestAccess: { true }, deviceAvailability: { nil }, start: { failure }
        )
        #expect(outcome == .failed(failure))
    }

    @Test func aPromptAnsweredAfterHangupLeavesNoNotice() async {
        var current = true
        let outcome = await CallCaptureAttempt.run(
            isCurrent: { current }, requestAccess: { current = false; return false },
            deviceAvailability: { Issue.record("The departed call can't look up devices"); return true },
            start: { Issue.record("The departed call can't start capture"); return nil }
        )
        #expect(outcome == .superseded)
    }

    @Test func aDeviceListThatReturnsAfterHangupCannotStartCapture() async {
        var current = true
        let outcome = await CallCaptureAttempt.run(
            isCurrent: { current }, requestAccess: { true }, deviceAvailability: { current = false; return true },
            start: { Issue.record("The departed call can't start capture"); return nil }
        )
        #expect(outcome == .superseded)
    }

    @Test func aFailureAfterMutingOrHangingUpLeavesNoNotice() async {
        var current = true
        let outcome = await CallCaptureAttempt.run(
            isCurrent: { current }, requestAccess: { true }, deviceAvailability: { true },
            start: { current = false; return .inUse }
        )
        #expect(outcome == .superseded)
    }
}
