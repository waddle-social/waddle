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

    @Test func theNoticeSaysWhatItMeantForTheCallAndWhatToDo() {
        let microphone = CallMediaIssue(kind: .microphone, reason: .denied)
        #expect(microphone.message.contains("joined as a listener"))
        #expect(microphone.message.contains("check again"))
        #expect(microphone.retryTitle == "Check again")
        let camera = CallMediaIssue(kind: .camera, reason: .denied)
        #expect(camera.message.contains("joined without video"))
        #expect(camera.retryTitle == "Check again")
        #expect(CallMediaIssue(kind: .screen, reason: .failed).retryTitle == "Share again")
    }

    @Test func aSurfaceWithOneLineShowsTheFirstNotice() {
        let report = Report(isMicrophoneBlocked: true, isCameraBlocked: true)
        #expect(CallMediaIssues.line(for: report) == CallMediaIssue(kind: .microphone, reason: .denied).message)
        #expect(CallMediaIssues.line(for: Report(didScreenShareFail: true)) == "That could not be shared. Try again.")
    }
}
