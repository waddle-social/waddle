import XCTest
import WaddleKit
@testable import Waddle

@MainActor
final class CallStageCommandsTests: XCTestCase {
    func testAMicrophoneToggleForThePreviousCallIsIgnored() async throws {
        let sample = await activeSession()
        let calls = sample.coordinator.calls
        let sid = try XCTUnwrap(calls.state.sid)
        XCTAssertTrue(calls.state.isEstablished)
        let media = FakeCallMedia()
        let commands = makeCommands(sid: sid, in: sample, media: media, prompts: CallStagePrompts())

        await calls.hangUp()
        await sample.answerCall(from: SampleSession.jo, video: true, sid: "replacement-call")
        XCTAssertEqual(calls.state.sid, "replacement-call")
        XCTAssertTrue(calls.state.isEstablished)

        let toggled = expectation(description: "the old stage changes the microphone")
        toggled.isInverted = true
        media.onCall = { _ in toggled.fulfill() }
        commands.perform(.toggleMicrophone)
        await fulfillment(of: [toggled], timeout: 0.25)
        XCTAssertTrue(media.calls.isEmpty)
    }

    func testStageChoicesAreKeptForTheirCall() async throws {
        let sample = await activeSession()
        let sid = try XCTUnwrap(sample.coordinator.calls.state.sid)
        let commands = makeCommands(sid: sid, in: sample, media: nil, prompts: CallStagePrompts())
        let stage = sample.callStage

        commands.perform(.setSize(.immersive))
        XCTAssertEqual(stage.stage(of: sid).size, .immersive)
        commands.perform(.setViewMode(.speaker))
        XCTAssertEqual(stage.stage(of: sid).presentation.viewMode, .speaker)
        commands.perform(.toggleSelfView)
        XCTAssertTrue(stage.stage(of: sid).presentation.isSelfViewHidden)
        commands.perform(.toggleParticipants)
        XCTAssertTrue(stage.stage(of: sid).showsParticipants)

        let other = stage.stage(of: "another-call")
        XCTAssertEqual(other.size, .split)
        XCTAssertEqual(other.presentation.viewMode, .gallery)
        XCTAssertFalse(other.presentation.isSelfViewHidden)
        XCTAssertFalse(other.showsParticipants)
    }

    func testTwoMicrophoneRetriesStartOneCheck() async throws {
        let sample = await activeSession()
        let sid = try XCTUnwrap(sample.coordinator.calls.state.sid)
        let media = FakeCallMedia()
        let prompts = CallStagePrompts()
        let commands = makeCommands(sid: sid, in: sample, media: media, prompts: prompts)
        let checked = expectation(description: "the microphone is checked")
        let checkedAgain = expectation(description: "another check starts")
        checkedAgain.isInverted = true
        media.onCall = { [weak media] _ in
            if media?.calls.count == 1 { checked.fulfill() } else { checkedAgain.fulfill() }
        }

        commands.perform(.retry(.microphone))
        XCTAssertEqual(prompts.checking, [.microphone])
        commands.perform(.retry(.microphone))

        await fulfillment(of: [checked, checkedAgain], timeout: 1)
        XCTAssertEqual(media.calls, [.setMicrophone(true)])
    }

    func testTogglingScreenShareStopsAnActiveOrStartingShare() async throws {
        let sample = await activeSession()
        let sid = try XCTUnwrap(sample.coordinator.calls.state.sid)

        for isOn in [true, false] {
            let media = FakeCallMedia()
            media.isScreenShareOn = isOn
            media.isScreenShareStarting = !isOn
            let commands = makeCommands(sid: sid, in: sample, media: media, prompts: CallStagePrompts())
            let called = expectation(description: "the share is stopped")
            media.onCall = { _ in called.fulfill() }

            commands.perform(.toggleScreenShare)

            await fulfillment(of: [called], timeout: 1)
            XCTAssertEqual(media.calls, [.stopScreenShare])
        }
    }

    func testTogglingScreenShareOffersSourcesWhenThereIsNoShare() async throws {
        let sample = await activeSession()
        let sid = try XCTUnwrap(sample.coordinator.calls.state.sid)
        let media = FakeCallMedia()
        let sources = CallScreenSources(.init(screens: [.init(number: 1, width: 800, height: 600, isMain: true)]), ownBundleID: nil)
        media.sources = sources
        let prompts = CallStagePrompts()
        let commands = makeCommands(sid: sid, in: sample, media: media, prompts: prompts)
        let called = expectation(description: "screen sources are requested")
        media.onCall = { _ in called.fulfill() }

        commands.perform(.toggleScreenShare)

        await fulfillment(of: [called], timeout: 1)
        XCTAssertEqual(media.calls, [.screenSources])
        XCTAssertEqual(prompts.screenPicker, sources)
    }

    func testDismissIssueRecordsItsCurrentRefusal() async throws {
        let sample = await activeSession()
        let sid = try XCTUnwrap(sample.coordinator.calls.state.sid)
        let media = FakeCallMedia()
        media.isMicrophoneBlocked = true
        media.mediaRefusals = [.microphone: 1]
        let commands = makeCommands(sid: sid, in: sample, media: media, prompts: CallStagePrompts())

        commands.perform(.dismissIssue(.microphone))

        let dismissed = sample.callStage.stage(of: sid).dismissedMediaIssues
        XCTAssertTrue(dismissed.hides(.microphone, in: media.mediaReport))
        media.mediaRefusals[.microphone] = 2
        XCTAssertFalse(dismissed.hides(.microphone, in: media.mediaReport))
    }

    private func activeSession() async -> SampleSession {
        let sample = SampleSession(account: SampleSession.anaOnThisMac)
        await sample.answerCall(from: SampleSession.priya, video: true)
        return sample
    }

    private func makeCommands(
        sid: String, in sample: SampleSession, media: FakeCallMedia?, prompts: CallStagePrompts
    ) -> CallStageCommands {
        CallStageCommands(
            sid: sid, session: sample.coordinator, media: media, stage: sample.callStage,
            navigation: sample.navigation, prompts: prompts, chat: nil
        )
    }
}
