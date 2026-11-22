import SwiftUI

/// The sign-in card (web `signin-many`): a bordered surface on iPad and
/// Mac, the plain column on iPhone.
struct SignInCard<Content: View>: View {
    @ViewBuilder let content: Content
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    var body: some View {
        let column = VStack(alignment: .leading, spacing: Theme.Spacing.l) { content }
            .frame(maxWidth: Theme.Size.signInColumn)

        if isCard {
            column
                .padding(Theme.Spacing.l)
                .huddleSurface(.card, radius: Theme.Radius.card)
                .frame(maxWidth: Theme.Size.signInColumn + Theme.Spacing.l * 2)
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
