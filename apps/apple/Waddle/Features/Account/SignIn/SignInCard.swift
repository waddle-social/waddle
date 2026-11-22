import SwiftUI

/// The sign-in card (web `signin-many`): a bordered surface on iPad and
/// Mac, the plain column on iPhone (Android `login`).
struct SignInCard<Content: View>: View {
    let layout: SignInLayout
    @ViewBuilder let content: Content

    var body: some View {
        let column = VStack(alignment: .leading, spacing: Theme.Spacing.l) { content }
            .frame(maxWidth: Theme.Size.signInColumn)

        if layout == .card {
            column
                .padding(Theme.Spacing.l)
                .huddleSurface(.card, radius: Theme.Radius.card)
                .frame(maxWidth: Theme.Size.signInColumn + Theme.Spacing.l * 2)
        } else {
            column
        }
    }
}
