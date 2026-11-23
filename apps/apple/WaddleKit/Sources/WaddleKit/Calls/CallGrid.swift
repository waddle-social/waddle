import Foundation

/// The web stage's fixed grid keeps tile sizes predictable as the split resizes.
public struct CallGrid: Equatable, Sendable {
    public let columns: Int
    public let rows: Int
    public var capacity: Int { columns * rows }

    private static let single = Self(columns: 1, rows: 1)
    private static let layouts: [(grid: Self, width: CGFloat, height: CGFloat)] = [
        (single, 0, 0),
        (Self(columns: 1, rows: 2), 0, 360),
        (Self(columns: 2, rows: 1), 360, 0),
        (Self(columns: 2, rows: 2), 480, 360),
        (Self(columns: 3, rows: 2), 720, 360),
        (Self(columns: 3, rows: 3), 720, 540),
        (Self(columns: 4, rows: 3), 960, 540),
        (Self(columns: 4, rows: 4), 960, 720),
        (Self(columns: 5, rows: 5), 1200, 900),
    ]

    public static func make(tileCount: Int, size: CGSize) -> Self {
        let width = max(0, size.width)
        let height = max(0, size.height)
        let fits = layouts.filter { width >= $0.width && height >= $0.height }.map(\.grid)
        let needed = max(1, tileCount)
        let isLandscape = width >= height
        func rank(_ grid: Self) -> (Int, Int) {
            (grid.capacity, (grid.columns >= grid.rows) == isLandscape ? 0 : 1)
        }

        // Orientation breaks ties only when the layout can fit every tile.
        return fits.filter { $0.capacity >= needed }.min { rank($0) < rank($1) }
            ?? fits.min { $0.capacity > $1.capacity }
            ?? single
    }
}
