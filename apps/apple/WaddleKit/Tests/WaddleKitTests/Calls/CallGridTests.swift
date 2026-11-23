import Foundation
import Testing

@testable import WaddleKit

@Suite("Call grid") struct CallGridTests {
    @Test(arguments: [
        (1, 1200, 800, 1, 1),
        (1, 320, 200, 1, 1),
        (2, 1200, 600, 2, 1),
        (2, 400, 900, 1, 2),
        (2, 360, 360, 2, 1),
        (4, 1000, 700, 2, 2),
        (6, 1100, 500, 3, 2),
        (9, 1100, 800, 3, 3),
        (12, 960, 540, 4, 3),
        (16, 960, 720, 4, 4),
        (25, 1600, 1100, 5, 5),
        (30, 1600, 1100, 5, 5),
        (9, 600, 400, 2, 2),
        (5, 400, 400, 1, 2),
        (8, 240, 180, 1, 1),
        (0, 800, 600, 1, 1),
        (-5, 800, 600, 1, 1),
        (6, 1100, 360, 3, 2),
        (6, 1100, 359, 2, 1),
    ])
    func choosesTheSmallestEnoughGridOrLargestThatFits(count: Int, width: Int, height: Int, columns: Int, rows: Int) {
        let grid = CallGrid.make(tileCount: count, size: CGSize(width: CGFloat(width), height: CGFloat(height)))
        #expect(grid.columns == columns)
        #expect(grid.rows == rows)
        #expect(grid.capacity == columns * rows)
    }
}
