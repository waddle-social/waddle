import SwiftUI
import WaddleKit

/// One camera on a call's stage (web `CallTile`): the picture, or the
/// person's avatar while there is none, under a name plate. A ring in the
/// live colour says they are speaking; a gold badge, that their hand is up.
/// Clicking pins the tile, and again unpins it.
///
/// Pure: the picture comes from `video`, which only a tile that is drawn
/// asks for, so a camera nobody sees is never fetched.
struct CallTileView: View {
    let tile: CallStageTile
    var isPinned = false
    var video: (CallStageTile) -> AnyView? = { _ in nil }
    let togglePin: @MainActor () -> Void

    @State private var isHovered = false

    var body: some View {
        Button(action: togglePin) {
            GeometryReader { cell in
                ZStack {
                    Theme.Colors.surface
                    if tile.hasVideo, let picture = video(tile) {
                        // The tile takes the click, wherever on the picture it lands.
                        picture.allowsHitTesting(false)
                    } else {
                        avatar(size: min(Metrics.avatarShare * min(cell.size.width, cell.size.height), Metrics.avatar))
                    }
                }
            }
            .clipShape(Metrics.shape)
            .overlay(alignment: .topLeading) {
                if tile.isHandRaised { hand }
            }
            .overlay(alignment: .bottomLeading) { plate }
            .background { outline }
            .contentShape(Metrics.shape)
        }
        .buttonStyle(CallTileButtonStyle())
        .onHover { isHovered = $0 }
        .huddleAnimation(.press, value: tile.isSpeaking)
        .accessibilityElement()
        .accessibilityLabel(Text(tile.spokenLabel(isPinned: isPinned)))
        .accessibilityAddTraits(.isButton)
        .accessibilityAction(named: Text(isPinned ? "Unpin" : "Pin"), togglePin)
    }

    /// Only an address with a localpart is a person whose avatar can be looked up.
    private func avatar(size: CGFloat) -> some View {
        CallPersonAvatar(name: tile.label, jid: tile.bareJID.flatMap { $0.localpart == nil ? nil : $0 }, size: size)
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

/// A tile is a picture, not a control with a pressed look: it only settles under the pointer.
private struct CallTileButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? HuddleMotion.pressScale : 1)
            .huddleAnimation(.press, value: configuration.isPressed)
    }
}

/// The last cell of a full gallery: how many people have no tile (web's "+N more").
struct CallOverflowTile: View {
    let hiddenCount: Int

    var body: some View {
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
        .accessibilityElement()
        .accessibilityLabel(Text(hiddenCount == 1 ? "1 more person" : "\(hiddenCount) more people"))
    }
}
