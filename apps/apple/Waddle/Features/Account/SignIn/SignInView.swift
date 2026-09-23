import SwiftUI

/// First-run and signed-out screen: pick a provider, then approve the
/// device code (RFC 8628) in the browser.
struct SignInView: View {
    @Environment(AppState.self) private var app

    init() {}

    var body: some View {
        GeometryReader { proxy in
            ScrollView {
                SignInCard {
                    SignInHeader()
                    content
                }
                .padding(.horizontal, Theme.Spacing.xl)
                .padding(.vertical, Theme.Spacing.xl * 2)
                .frame(maxWidth: .infinity, minHeight: proxy.size.height)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
        .background { SignInBackdrop() }
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 560)
        #endif
    }

    @ViewBuilder
    private var content: some View {
        if case let .authorizing(authorization) = app.phase {
            SignInAuthorizingPanel(authorization: authorization)
                .transition(.opacity)
        } else {
            SignInProviderPanel()
                .transition(.opacity)
            SignInServerRow()
        }
    }
}

/// Brand mark, name and tagline.
private struct SignInHeader: View {
    var body: some View {
        VStack(spacing: Theme.Spacing.m) {
            WaddleBrandMark(size: 72)
            Text("Waddle")
                .font(.system(.largeTitle, design: .rounded).weight(.bold))
            Text("Chat and communities, built on open standards.")
                .font(.body)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
        }
        .accessibilityElement(children: .combine)
        .padding(.bottom, Theme.Spacing.s)
    }
}

/// Soft brand-tinted wash behind the card.
private struct SignInBackdrop: View {
    var body: some View {
        LinearGradient(
            colors: [Color.accentColor.opacity(0.16), Color.primaryBackground],
            startPoint: .top,
            endPoint: .center
        )
        .background(Color.primaryBackground)
        .ignoresSafeArea()
    }
}
