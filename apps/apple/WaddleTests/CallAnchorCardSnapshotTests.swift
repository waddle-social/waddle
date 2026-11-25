import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

/// A room's call announcement as a card in its conversation.
@MainActor
final class CallAnchorCardSnapshotTests: XCTestCase {
    private let anchor = CallThreadAnchor(kind: .room, hasVideo: false)

    private func assertCard(_ card: CallAnchorCard, width: CGFloat = 520, testName: String = #function, line: UInt = #line) {
        assertHuddleSnapshot(
            precision: 0.97,
            of: CallAnchorCardView(card: card, time: "14:32") { /* nothing to join */ }
                .frame(width: width, alignment: .leading),
            width: width + 2 * Theme.Spacing.l,
            testName: testName,
            line: line
        )
    }

    private func live(_ people: [String], video: Bool = false, ours: Bool = false, busy: Bool = false, canJoin: Bool = true) -> CallAnchorCard {
        CallAnchorCard.make(
            anchor: anchor, said: "ada started a call", ended: nil,
            live: .init(people: people, hasVideo: video, isOurs: ours, isBusy: busy, canJoin: canJoin), isNewest: true
        )
    }

    func testALiveCallToJoin() {
        assertCard(live(["Ada", "Linus", "Grace", "Margaret"], video: true))
    }

    func testTheCallWeAreIn() {
        assertCard(live(["You", "Ada"], ours: true, canJoin: false))
    }

    func testWhileInAnotherCall() {
        assertCard(live(["Ada", "Linus"], busy: true, canJoin: false))
    }

    func testACallThatEnded() {
        assertCard(CallAnchorCard.make(
            anchor: anchor, said: "ada started a call", ended: CallThreadEnded(anchorID: "o1", duration: 754), live: nil,
            isNewest: true
        ))
    }

    func testALiveCallInANarrowConversation() {
        assertCard(live(["Ada", "Linus", "Grace", "Margaret", "Alan"]), width: 320)
    }
}
