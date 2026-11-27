import Foundation
import Testing
@testable import WaddleKit

struct ComposerBubblePlacementTests {
    private let content = CGRect(x: 100, y: 100, width: 400, height: 300)
    private let screen = CGRect(x: 0, y: 0, width: 600, height: 600)
    private let size = CGSize(width: 100, height: 30)

    private func place(_ anchor: CGRect, screen: CGRect? = nil) -> CGRect? {
        ComposerBubblePlacement.frame(
            for: size, selection: anchor, windowContent: content, screenVisible: screen ?? self.screen, gap: 8
        )
    }

    @Test func centersAboveSelectionInScreenCoordinates() {
        #expect(place(CGRect(x: 230, y: 140, width: 40, height: 20))
            == CGRect(x: 200, y: 168, width: 100, height: 30))
    }

    @Test func flipsBelowWhenThereIsNoRoomAbove() {
        #expect(place(CGRect(x: 230, y: 370, width: 40, height: 20))
            == CGRect(x: 200, y: 332, width: 100, height: 30))
    }

    @Test func clampsToTheIntersectionOfWindowContentAndTheChosenScreen() {
        let otherScreen = CGRect(x: 300, y: 0, width: 130, height: 155)
        #expect(place(CGRect(x: 410, y: 112, width: 14, height: 20), screen: otherScreen)
            == CGRect(x: 330, y: 100, width: 100, height: 30))
        #expect(place(CGRect(x: 100, y: 140, width: 4, height: 20))
            == CGRect(x: 100, y: 168, width: 100, height: 30))
    }

    @Test func hidesWhenTheBubbleCannotFitOrTheWindowIsOffScreen() {
        let anchor = CGRect(x: 230, y: 140, width: 40, height: 20)
        #expect(place(anchor, screen: CGRect(x: 300, y: 100, width: 90, height: 300)) == nil)
        #expect(place(anchor, screen: CGRect(x: 800, y: 100, width: 300, height: 300)) == nil)
    }
}
