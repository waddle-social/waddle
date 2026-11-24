import SwiftUI
import WaddleKit

/// Where a stage's tiles get their pictures and send a pin. One object for
/// the life of the session, so a tile holds on to it across redraws and is
/// compared by its values alone.
@MainActor
final class CallTileSource {
    /// A camera's picture, while there is one.
    let video: @MainActor (CallStageTile) -> AnyView?
    let togglePin: @MainActor (CallStageTile.ID) -> Void

    init(
        video: @escaping @MainActor (CallStageTile) -> AnyView?,
        togglePin: @escaping @MainActor (CallStageTile.ID) -> Void
    ) {
        self.video = video
        self.togglePin = togglePin
    }
}

/// One camera on a call's stage (web `CallTile`): the picture, or the
/// person's avatar while there is none, under a name plate. A ring in the
/// live colour says they are speaking; a gold badge, that their hand is up.
/// Clicking pins the tile, and again unpins it.
///
/// Equatable, and drawn with `.equatable()`: the stage redraws whenever
/// anyone starts or stops speaking, and on a Mac every picture is drawn on
/// the main thread (#2625), so a tile whose values did not change must cost nothing.
struct CallTileView: View, Equatable {
    let tile: CallStageTile
    var isPinned = false
    let source: CallTileSource

    @State private var isHovered = false

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.tile == rhs.tile && lhs.isPinned == rhs.isPinned && lhs.source === rhs.source
    }

    var body: some View {
        // The picture stands outside the button that takes the click: a
        // button redraws its label with it, and the picture must not be.
        CallTilePicture(tile: tile, source: source)
            .equatable()
            .clipShape(Metrics.shape)
            .overlay(alignment: .topLeading) {
                if tile.isHandRaised { hand }
            }
            .overlay(alignment: .bottomLeading) { plate }
            .background { outline }
            .huddleAnimation(.press, value: tile.isSpeaking)
            .accessibilityHidden(true)
            .overlay {
                Button { source.togglePin(tile.id) } label: {
                    Color.clear.contentShape(Metrics.shape)
                }
                .buttonStyle(.plain)
                .onHover { isHovered = $0 }
                .accessibilityLabel(Text(tile.spokenLabel(isPinned: isPinned)))
                .accessibilityAction(named: Text(isPinned ? "Unpin" : "Pin")) { source.togglePin(tile.id) }
            }
    }

    /// Drawn around the tile, as the web's box shadows are, so a ring never covers the picture.
    @ViewBuilder
    private var outline: some View {
        if tile.isSpeaking {
            Metrics.shape.inset(by: -Metrics.ring - Metrics.halo)
                .strokeBorder(Theme.Colors.live.opacity(0.22), lineWidth: Metrics.halo)
            Metrics.shape.inset(by: -Metrics.ring)
                .strokeBorder(Theme.Colors.live, lineWidth: Metrics.ring)
        } else {
            Metrics.shape.inset(by: -WaddleTokens.BorderWidth.hair)
                .strokeBorder(isHovered ? Theme.Colors.borderStrong : Theme.Colors.borderSoft, lineWidth: WaddleTokens.BorderWidth.hair)
        }
    }

    private var plate: some View {
        HStack(spacing: WaddleTokens.Spacing.s1Half) {
            if tile.source == .screen {
                // The web's presenting glyph: this tile is a screen, not a person.
                Image(systemName: "rectangle.inset.filled.and.person.filled")
                    .font(.system(size: Metrics.plateGlyph, weight: .semibold))
            }
            Text(tile.label)
                .huddleText(Theme.Typography.label.sized(WaddleTokens.FontSize.xs))
                .lineLimit(1)
            if tile.isMuted {
                Image(systemName: "mic.slash")
                    .font(.system(size: Metrics.plateGlyph, weight: .semibold))
                    .foregroundStyle(Theme.Colors.dangerText)
            }
        }
        .foregroundStyle(Theme.Material.mediaFg)
        .padding(.horizontal, WaddleTokens.Spacing.s2Half)
        .frame(minHeight: Metrics.plate)
        .background(Capsule().fill(Theme.Material.media))
        .padding(Theme.Spacing.s)
    }

    private var hand: some View {
        Image(systemName: "hand.raised")
            .font(.system(size: Metrics.handGlyph, weight: .semibold))
            .foregroundStyle(Theme.Colors.goldFg)
            .frame(width: Metrics.hand, height: Metrics.hand)
            .background(Circle().fill(Theme.Colors.gold))
            .padding(Theme.Spacing.s)
    }

    enum Metrics {
        static let shape = RoundedRectangle(cornerRadius: Theme.Radius.medium, style: .continuous)
        /// The avatar's share of the tile's shorter side, up to `avatar`.
        static let avatarShare: CGFloat = 0.36
        static let avatar: CGFloat = 96
        static let plate: CGFloat = 26
        static let plateGlyph: CGFloat = 12
        static let hand: CGFloat = 28
        static let handGlyph: CGFloat = 14
        static let ring = WaddleTokens.BorderWidth.strong
        static let halo: CGFloat = 4
        /// How far a speaking tile draws beyond its own frame.
        static let overdraw = ring + halo
    }
}

/// What fills a tile: the camera's picture, or the person's avatar while
/// there is none. It changes only with the camera, never with a badge or
/// the speaking ring, so a video view is left alone while its tile's ring comes and goes.
///
/// Only a picture that is drawn asks the source for video, so a camera nobody sees is never fetched.
private struct CallTilePicture: View, Equatable {
    let tile: CallStageTile
    let source: CallTileSource

    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.tile.id == rhs.tile.id && lhs.tile.hasVideo == rhs.tile.hasVideo && lhs.tile.label == rhs.tile.label
            && lhs.source === rhs.source
    }

    var body: some View {
        ZStack {
            Theme.Colors.surface
            // The source is asked outside any GeometryReader: a reader's content
            // is rebuilt on every size pass, as when a window settles into its
            // size, and each rebuild would fetch the picture again.
            if tile.hasVideo, let picture = source.video(tile) {
                // The tile takes the click, wherever on the picture it lands.
                picture.allowsHitTesting(false)
            } else {
                GeometryReader { cell in
                    let side = min(cell.size.width, cell.size.height)
                    // Only an address with a localpart is a person whose avatar can be looked up.
                    CallPersonAvatar(
                        name: tile.label, jid: tile.bareJID.flatMap { $0.localpart == nil ? nil : $0 },
                        size: min(CallTileView.Metrics.avatarShare * side, CallTileView.Metrics.avatar)
                    )
                    // A reader places its content at the top leading corner.
                    .frame(width: cell.size.width, height: cell.size.height)
                }
            }
        }
    }
}

/// The last cell of a full gallery: how many people have no tile (web's "+N more").
struct CallOverflowTile: View {
    let hiddenCount: Int
    /// Opens the list of everyone, or closes it when it is open.
    var toggleParticipants: @MainActor () -> Void = { /* a stage with no panel */ }

    var body: some View {
        Button(action: toggleParticipants) { face }
            .buttonStyle(.plain)
            .accessibilityLabel(Text(hiddenCount == 1 ? "1 more person" : "\(hiddenCount) more people"))
            .accessibilityHint(Text("Shows or hides everyone in the call"))
            .help("Everyone in the call")
    }

    private var face: some View {
        VStack(spacing: Theme.Spacing.xs) {
            Text("+\(hiddenCount)")
                .huddleText(Theme.Typography.title.sized(22).weight(700))
                .foregroundStyle(Theme.Colors.fg)
            Text("more")
                .huddleText(Theme.Typography.body.sized(WaddleTokens.FontSize.sm))
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(CallTileView.Metrics.shape.fill(Theme.Colors.surface2))
        .contentShape(CallTileView.Metrics.shape)
    }
}
