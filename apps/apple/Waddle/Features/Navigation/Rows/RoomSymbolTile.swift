import SwiftUI

/// A room glyph on a quiet rounded tile, sized like an avatar.
struct RoomSymbolTile: View {
    let symbol: String
    var size: CGFloat = Theme.Size.avatar

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.3, style: .continuous)
            .fill(Theme.Colors.surface2)
            .frame(width: size, height: size)
            .overlay {
                Image(systemName: symbol)
                    .font(.system(size: size * 0.45, weight: .semibold))
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .accessibilityHidden(true)
    }
}
