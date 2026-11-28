import Testing

@testable import WaddleKit

@Suite("Call media issues") struct CallMediaIssuesTests {
    private typealias Report = CallMediaIssues.Report

    @Test func aCallWithNothingWrongHasNoNotice() {
        #expect(CallMediaIssues.shown(for: Report()).isEmpty)
        #expect(CallMediaIssues.line(for: Report()) == nil)
    }

    @Test func aFailedCaptureExplainsWhatHappenedToTheCall() {
        let microphone = CallMediaIssue(kind: .microphone, reason: .failed)
        let camera = CallMediaIssue(kind: .camera, reason: .failed)
        #expect(microphone.message() == "Couldn't start your microphone — you joined as a listener.")
        #expect(camera.message() == "Couldn't start your camera — you joined without video.")
    }

    @Test func aMissingCameraAndBusyMicrophoneSayHowToRecover() {
        let report = Report(microphoneIssue: .inUse, cameraIssue: .missing)
        let issues = CallMediaIssues.shown(for: report)
        #expect(issues == [CallMediaIssue(kind: .microphone, reason: .inUse), CallMediaIssue(kind: .camera, reason: .missing)])
        #expect(issues.map { $0.message(on: .mac) } == [
            "Your microphone is in use by another app. Close it, then check again.",
            "No camera detected. Connect one, then check again.",
        ])
        #expect(issues.allSatisfy { !$0.opensSettings && $0.retryTitle == "Check again" })
        #expect(CallMediaIssues.line(for: report) == "Your microphone is in use by another app.")
    }

    @Test func aDifferentFailureBringsBackADismissedNotice() {
        var report = Report(cameraIssue: .missing, refusals: [.camera: 1])
        var dismissed = CallMediaIssues.Dismissals()
        dismissed.dismiss(.camera, in: report)
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed).isEmpty)
        report.cameraIssue = .inUse
        report.refusals[.camera] = 2
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed) == [CallMediaIssue(kind: .camera, reason: .inUse)])
    }

    @Test func eachBlockedDeviceHasItsOwnNoticeTheMicrophonesFirst() {
        let issues = CallMediaIssues.shown(for: Report(microphoneIssue: .denied, cameraIssue: .denied, isScreenBlocked: true))
        #expect(issues.map(\.kind) == [.microphone, .camera, .screen])
        #expect(issues.allSatisfy { $0.reason == .denied })
    }

    @Test func aBlockedScreenOutranksAShareThatFailed() {
        #expect(CallMediaIssues.shown(for: Report(isScreenBlocked: true, didScreenShareFail: true))
            == [CallMediaIssue(kind: .screen, reason: .denied)])
        #expect(CallMediaIssues.shown(for: Report(didScreenShareFail: true)) == [CallMediaIssue(kind: .screen, reason: .failed)])
    }

    @Test func aNoticePutAwayIsLeftOutAndTheOthersStay() {
        let report = Report(microphoneIssue: .denied, cameraIssue: .denied, refusals: [.microphone: 1, .camera: 1])
        var dismissed = CallMediaIssues.Dismissals()
        dismissed.dismiss(.camera, in: report)
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed).map(\.kind) == [.microphone])
        dismissed.dismiss(.microphone, in: report)
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed).isEmpty)
    }

    /// Whichever surface asked: the notice knows only that the device was refused once more.
    @Test func aDeviceRefusedAgainBringsItsNoticeBack() {
        var report = Report(microphoneIssue: .denied, refusals: [.microphone: 1])
        var dismissed = CallMediaIssues.Dismissals()
        dismissed.dismiss(.microphone, in: report)
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed).isEmpty)
        report.refusals[.microphone] = 2
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed).map(\.kind) == [.microphone])
        // Put away again, it stays away until the next refusal.
        dismissed.dismiss(.microphone, in: report)
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed).isEmpty)
    }

    @Test func aShareThatFailsAgainAfterItsNoticeWasPutAwayIsSaidAgain() {
        var report = Report(didScreenShareFail: true, refusals: [.screen: 1])
        var dismissed = CallMediaIssues.Dismissals()
        dismissed.dismiss(.screen, in: report)
        report.refusals[.screen] = 2
        #expect(CallMediaIssues.shown(for: report, dismissed: dismissed) == [CallMediaIssue(kind: .screen, reason: .failed)])
    }

    @Test func theNoticeSaysWhatItMeansAndWhatCanBeDoneOnThisKindOfDevice() {
        let microphone = CallMediaIssue(kind: .microphone, reason: .denied)
        #expect(microphone.message(on: .mac).contains("nobody hears you"))
        #expect(microphone.message(on: .mac).contains("then check again"))
        // A phone closes the app when the switch is changed: checking again is not something it can offer.
        #expect(!microphone.message(on: .phone).contains("check again"))
        #expect(microphone.message(on: .phone).contains("join again"))
        #expect(CallMediaIssue(kind: .camera, reason: .denied).message(on: .mac).contains("nobody sees you"))
        #expect(microphone.retryTitle == "Check again")
        #expect(CallMediaIssue(kind: .screen, reason: .failed).retryTitle == "Share again")
    }

    @Test func onlyARefusalIsPutRightInSettings() {
        #expect(CallMediaIssue(kind: .microphone, reason: .denied).opensSettings)
        #expect(CallMediaIssue(kind: .screen, reason: .denied).opensSettings)
        #expect(!CallMediaIssue(kind: .screen, reason: .failed).opensSettings)
    }

    @Test func aSurfaceWithOneLineShowsTheFirstNoticeInWordsThatPromiseNoButton() {
        let report = Report(microphoneIssue: .denied, cameraIssue: .denied)
        let line = CallMediaIssues.line(for: report)
        #expect(line == "Waddle is not allowed to use the microphone. Change that in Settings.")
        #expect(line?.contains("check again") == false)
        #expect(CallMediaIssues.line(for: Report(didScreenShareFail: true)) == "That could not be shared. Try again.")
    }

    @Test func aBlurThatIsNotWorkingSaysThePictureIsHeldBack() {
        let issues = CallMediaIssues.shown(for: Report(isBackgroundFailing: true))
        #expect(issues == [CallMediaIssue(kind: .background, reason: .failed)])
        #expect(issues[0].message().contains("held back"))
        #expect(!issues[0].opensSettings)
        #expect(issues[0].retryTitle == "Check again")
    }
}
