import Testing

@testable import WaddleKit

@Suite("Call media issues") struct CallMediaIssuesTests {
    private typealias Report = CallMediaIssues.Report

    @Test func aCallWithNothingWrongHasNoNotice() {
        #expect(CallMediaIssues.shown(for: Report()).isEmpty)
        #expect(CallMediaIssues.line(for: Report()) == nil)
    }

    @Test func eachBlockedDeviceHasItsOwnNoticeTheMicrophonesFirst() {
        let issues = CallMediaIssues.shown(for: Report(isMicrophoneBlocked: true, isCameraBlocked: true, isScreenBlocked: true))
        #expect(issues.map(\.kind) == [.microphone, .camera, .screen])
        #expect(issues.allSatisfy { $0.reason == .denied })
    }

    @Test func aBlockedScreenOutranksAShareThatFailed() {
        #expect(CallMediaIssues.shown(for: Report(isScreenBlocked: true, didScreenShareFail: true))
            == [CallMediaIssue(kind: .screen, reason: .denied)])
        #expect(CallMediaIssues.shown(for: Report(didScreenShareFail: true)) == [CallMediaIssue(kind: .screen, reason: .failed)])
    }

    @Test func aNoticePutAwayIsLeftOutAndTheOthersStay() {
        let report = Report(isMicrophoneBlocked: true, isCameraBlocked: true)
        #expect(CallMediaIssues.shown(for: report, dismissed: [.camera]).map(\.kind) == [.microphone])
        #expect(CallMediaIssues.shown(for: report, dismissed: [.microphone, .camera]).isEmpty)
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
        let report = Report(isMicrophoneBlocked: true, isCameraBlocked: true)
        let line = CallMediaIssues.line(for: report)
        #expect(line == "Waddle is not allowed to use the microphone. Change that in Settings.")
        #expect(line?.contains("check again") == false)
        #expect(CallMediaIssues.line(for: Report(didScreenShareFail: true)) == "That could not be shared. Try again.")
    }
}
