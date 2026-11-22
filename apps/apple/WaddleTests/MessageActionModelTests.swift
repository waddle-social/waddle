import WaddleKit
import XCTest
@testable import Waddle

/// Which row shows its hover toolbar, and which row's words a Mac lets
/// one select.
@MainActor
final class MessageActionModelTests: XCTestCase {
    private func model() -> MessageActionModel {
        MessageActionModel(
            conversation: .room(BareJID(parsing: "lobby@muc.waddle.test")!),
            composer: ComposerModel(),
            thread: nil
        )
    }

    func testAHoveredRowShowsItsToolbar() {
        let actions = model()
        XCTAssertTrue(actions.showsToolbar(of: "a", isRowHovered: true))
        XCTAssertFalse(actions.showsToolbar(of: "a", isRowHovered: false))
    }

    /// A toolbar floats over the row above its own: while the pointer is
    /// on it, that row does not show a second one.
    func testAHeldToolbarIsTheOnlyOne() {
        let actions = model()
        actions.holdToolbar(of: "b")
        XCTAssertTrue(actions.showsToolbar(of: "b", isRowHovered: false))
        XCTAssertFalse(actions.showsToolbar(of: "a", isRowHovered: true))

        actions.releaseToolbar(of: "b")
        XCTAssertTrue(actions.showsToolbar(of: "a", isRowHovered: true))
    }

    /// A row scrolled away or removed while its toolbar was held releases
    /// it; a release for another row changes nothing.
    func testOnlyTheHolderReleases() {
        let actions = model()
        actions.holdToolbar(of: "b")
        actions.releaseToolbar(of: "a")
        XCTAssertEqual(actions.heldToolbarRow, "b")
        actions.releaseToolbar(of: "b")
        XCTAssertNil(actions.heldToolbarRow)
    }

    func testSelectTextLastsUntilThatRowEndsIt() {
        let actions = model()
        actions.selectText(of: "a")
        actions.selectText(of: "b")
        XCTAssertEqual(actions.selectableRow, "b")
        actions.endTextSelection(of: "a")
        XCTAssertEqual(actions.selectableRow, "b")
        actions.endTextSelection(of: "b")
        XCTAssertNil(actions.selectableRow)
    }
}
