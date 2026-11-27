import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class ConversationDropSnapshotTests: XCTestCase {
    /// The overlay alone: it covers its pane, and a whole conversation behind it
    /// would only make the picture depend on things this test is not about.
    func testFileDragOverConversationPane() async throws {
        let pane = Theme.Colors.bg
            .overlay { ConversationDropOverlay() }
        try await assertScreenSnapshot(of: SampleSession().host(pane), size: CGSize(width: 420, height: 320))
    }
}
