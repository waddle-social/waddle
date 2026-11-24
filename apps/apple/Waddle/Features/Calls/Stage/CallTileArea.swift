import SwiftUI
import WaddleKit

/// The tiles of a call's stage (web `CallTileGrid`), laid out by
/// `CallStageLayout` for the room there is: an even gallery that ends in
/// "+N more" when it is full, or one large tile over a strip of the rest.
struct CallTileArea: View {
    let tiles: [CallStageTile]
    let presentation: CallStagePresentation
    var promoted: String?
    let source: CallTileSource
    /// Opens the list of everyone, for the people the gallery has no cell for.
    var showParticipants: (@MainActor () -> Void)?

    /// The most gallery cells, the "+N more" cell included: eight people and
    /// the rest. Set by a measurement still to be done: on a Mac LiveKit
    /// draws every video on the main thread (#2625), so the number of
    /// cameras drawn at once is what the cap protects.
    static let maxTiles = 9

    var body: some View {
        GeometryReader { area in
            switch CallStageLayout.make(
                tiles: tiles, presentation: presentation, promoted: promoted, size: area.size, maxTiles: Self.maxTiles
            ) {
            case let .grid(columns, rows, tiles, hiddenCount):
                gallery(tiles, hiddenCount: hiddenCount, columns: columns, rows: rows, in: area.size)
            case let .spotlight(main, strip):
                spotlight(main, strip: strip, width: area.size.width)
            }
        }
    }

    private func tile(_ tile: CallStageTile) -> some View {
        // Compared by value: a change to one tile leaves the others, and their pictures, alone.
        CallTileView(tile: tile, isPinned: tile.id == presentation.pinnedTileID, source: source).equatable()
    }

    // MARK: - Gallery

    private enum Cell: Identifiable {
        case tile(CallStageTile)
        case overflow(Int)

        var id: CallStageTile.ID? {
            if case .tile(let tile) = self { tile.id } else { nil }
        }
    }

    /// Every cell the same size, filled row by row, as the web's grid does.
    private func gallery(_ tiles: [CallStageTile], hiddenCount: Int, columns: Int, rows: Int, in size: CGSize) -> some View {
        let cells = tiles.map(Cell.tile) + (hiddenCount > 0 ? [.overflow(hiddenCount)] : [])
        let width = max(0, (size.width - Metrics.gap * CGFloat(columns - 1)) / CGFloat(columns))
        let height = max(0, (size.height - Metrics.gap * CGFloat(rows - 1)) / CGFloat(rows))
        return VStack(alignment: .leading, spacing: Metrics.gap) {
            ForEach(0..<rows, id: \.self) { row in
                HStack(spacing: Metrics.gap) {
                    ForEach(cells.dropFirst(row * columns).prefix(columns)) { cell in
                        Group {
                            switch cell {
                            case .tile(let tile): self.tile(tile)
                            case .overflow(let count): CallOverflowTile(hiddenCount: count, showParticipants: showParticipants)
                            }
                        }
                        .frame(width: width, height: height)
                    }
                }
            }
        }
        .frame(width: size.width, height: size.height, alignment: .topLeading)
    }

    // MARK: - Spotlight

    private func spotlight(_ main: CallStageTile, strip: [CallStageTile], width: CGFloat) -> some View {
        VStack(alignment: .leading, spacing: Metrics.stripGap) {
            tile(main)
            if !strip.isEmpty {
                self.strip(strip, width: width)
            }
        }
    }

    /// The other tiles as thumbnails. A strip that fits is laid out plainly;
    /// a longer one scrolls and builds a thumbnail only as it comes into
    /// view, so a camera off screen is not drawn.
    @ViewBuilder
    private func strip(_ tiles: [CallStageTile], width: CGFloat) -> some View {
        let thumbnails = ForEach(tiles) { tile in
            self.tile(tile).frame(width: Metrics.thumbnail.width, height: Metrics.thumbnail.height)
        }
        let needed = CGFloat(tiles.count) * (Metrics.thumbnail.width + Metrics.gap) - Metrics.gap
        if needed <= width {
            HStack(spacing: Metrics.gap) { thumbnails }
        } else {
            // A speaking ring draws beyond its tile: give it room inside the clip.
            ScrollView(.horizontal) {
                LazyHStack(spacing: Metrics.gap) { thumbnails }
                    .padding(CallTileView.Metrics.overdraw)
            }
            .scrollIndicators(.hidden)
            .frame(height: Metrics.thumbnail.height + 2 * CallTileView.Metrics.overdraw)
            .padding(-CallTileView.Metrics.overdraw)
        }
    }

    private enum Metrics {
        static let gap = WaddleTokens.Spacing.s2Half
        /// Between the large tile and the strip under it.
        static let stripGap: CGFloat = 22
        static let thumbnail = CGSize(width: 144, height: 81)
    }
}
