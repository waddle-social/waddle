import XCTest
import WaddleKit

@testable import Waddle

@MainActor
final class CallStageModelTests: XCTestCase {
    private let lounge = ConversationID.room(BareJID(parsing: "lounge@rooms.waddle.test")!)
    private let priya = ConversationID.direct(BareJID(parsing: "priya@waddle.test")!)

    private func makeDefaults() throws -> UserDefaults {
        let name = "CallStageModelTests.\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        addTeardownBlock { defaults.removePersistentDomain(forName: name) }
        return defaults
    }

    func testADraggedSplitIsKeptForItsConversationAcrossLaunches() throws {
        let defaults = try makeDefaults()
        let model = CallStageModel(calls: SampleSession().coordinator.calls, media: nil, defaults: defaults)
        XCTAssertNil(model.splitPercent(for: lounge))

        model.setSplitPercent(62, for: lounge)
        model.setSplitPercent(5, for: priya)
        model.saveSplitPercents()

        let relaunched = CallStageModel(calls: SampleSession().coordinator.calls, media: nil, defaults: defaults)
        XCTAssertEqual(relaunched.splitPercent(for: lounge), 62)
        XCTAssertEqual(relaunched.splitPercent(for: priya), 25, "a share is kept within what a pane can give")
    }

    func testAStageKeepsItsSizeForItsCall() throws {
        let model = CallStageModel(calls: SampleSession().coordinator.calls, media: nil, defaults: try makeDefaults())
        model.update("a") { $0.size = .expanded }
        XCTAssertEqual(model.stage(of: "a").size, .expanded)
        XCTAssertEqual(model.stage(of: "b").size, .split)
    }

    func testEachWindowPlacesTheCallByItsOwnSidebar() async throws {
        let sample = SampleSession(account: SampleSession.anaOnThisMac)
        await sample.answerCall(from: SampleSession.priya, video: true)
        let state = sample.coordinator.calls.state
        let withSidebar = CallWindowLayout()
        let withoutSidebar = CallWindowLayout()
        withoutSidebar.sidebarColumnIsShown = false

        // Away from the call's conversation: a card where there is a sidebar, the dock where there is none.
        XCTAssertEqual(sample.callStage.surface(of: state, open: nil, platform: .regularWidth, in: withSidebar), .sidebarCard)
        XCTAssertEqual(sample.callStage.surface(of: state, open: nil, platform: .regularWidth, in: withoutSidebar), .dock)
        // One window showing its sidebar again says nothing about the other.
        withSidebar.sidebarIsOnScreen = true
        XCTAssertEqual(sample.callStage.surface(of: state, open: nil, platform: .regularWidth, in: withoutSidebar), .dock)
    }
}
