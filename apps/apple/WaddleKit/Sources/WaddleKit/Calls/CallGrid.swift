import Foundation

/// The web stage's fixed grid keeps tile sizes predictable as the split resizes.
public struct CallGrid: Equatable, Sendable {
    public let columns: Int
    public let rows: Int
    public var capacity: Int { columns * rows }

    /// How closely tiles may stand.
    public enum Density: Equatable, Sendable {
        /// The web's grid: a window's worth of room for every tile.
        case window
        /// A phone's screen: smaller cells, so four people fit upright and
        /// a row of four on its side, where the web's grid would show two.
        case phone
    }

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

    /// A cell at least 150 points each way.
    private static let phoneLayouts: [(grid: Self, width: CGFloat, height: CGFloat)] = [
        (single, 0, 0),
        (Self(columns: 1, rows: 2), 0, 300),
        (Self(columns: 2, rows: 1), 300, 0),
        (Self(columns: 3, rows: 1), 450, 0),
        (Self(columns: 2, rows: 2), 300, 300),
        (Self(columns: 4, rows: 1), 600, 0),
        (Self(columns: 2, rows: 3), 300, 450),
        (Self(columns: 3, rows: 2), 450, 300),
        (Self(columns: 2, rows: 4), 300, 600),
        (Self(columns: 4, rows: 2), 600, 300),
    ]

    public static func make(tileCount: Int, size: CGSize, density: Density = .window) -> Self {
        let width = max(0, size.width)
        let height = max(0, size.height)
        let layouts = density == .phone ? phoneLayouts : layouts
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
