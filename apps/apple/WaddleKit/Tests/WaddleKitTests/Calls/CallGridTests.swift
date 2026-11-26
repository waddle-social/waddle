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

    /// An iPhone's tile area upright (about 360 by 560) and on its side (about 730 by 230).
    @Test(arguments: [
        (1, 360, 560, 1, 1),
        (2, 360, 560, 1, 2),
        (3, 360, 560, 2, 2),
        (4, 360, 560, 2, 2),
        (6, 360, 560, 2, 3),
        // Eight upright need more height than a phone has under its header: six cells, the last "+N more".
        (9, 360, 560, 2, 3),
        (9, 360, 640, 2, 4),
        (2, 730, 230, 2, 1),
        (3, 730, 230, 3, 1),
        (4, 730, 230, 4, 1),
        (9, 730, 230, 4, 1),
        (4, 280, 280, 1, 1),
    ])
    func aPhoneStandsItsTilesCloser(count: Int, width: Int, height: Int, columns: Int, rows: Int) {
        let grid = CallGrid.make(
            tileCount: count, size: CGSize(width: CGFloat(width), height: CGFloat(height)), density: .phone
        )
        #expect(grid.columns == columns)
        #expect(grid.rows == rows)
    }
}
