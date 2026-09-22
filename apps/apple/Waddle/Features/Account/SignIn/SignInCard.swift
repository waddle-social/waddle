import SwiftUI

/// The centered sign-in column: plain on iPhone, a floating card on iPad
/// and Mac.
struct SignInCard<Content: View>: View {
    @ViewBuilder let content: Content
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    private static var maxWidth: CGFloat { 420 }

    var body: some View {
        let column = VStack(spacing: Theme.Spacing.xl) { content }
            .frame(maxWidth: Self.maxWidth)

        if isCard {
            column
                .padding(Theme.Spacing.xl * 1.5)
                .waddleGlass(in: RoundedRectangle(cornerRadius: 28, style: .continuous))
                .shadow(color: .black.opacity(0.08), radius: 24, y: 8)
                .frame(maxWidth: Self.maxWidth + Theme.Spacing.xl * 3)
        } else {
            column
        }
    }

    private var isCard: Bool {
        #if os(iOS)
        return sizeClass == .regular
        #else
        return true
        #endif
    }
}
