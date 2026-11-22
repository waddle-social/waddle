import SwiftUI

/// First-run and signed-out screen (web `signin-many`): pick a provider,
/// then approve the device code (RFC 8628) in the browser.
struct SignInView: View {
    @Environment(AppState.self) private var app

    init() {}

    var body: some View {
        SignInScreen(
            server: app.server,
            stage: SignInScreen.Stage(app),
            errorMessage: app.errorMessage
        )
    }
}

/// The sign-in screen for a given state, so it can be drawn without a
/// server. Its buttons still act through `AppState`.
struct SignInScreen: View {
    enum Stage: Equatable {
        case loadingProviders
        case providers([AuthProvider])
        case authorizing(DeviceAuthorization)

        @MainActor
        init(_ app: AppState) {
            if case let .authorizing(authorization) = app.phase {
                self = .authorizing(authorization)
            } else if app.isLoadingProviders {
                self = .loadingProviders
            } else {
                self = .providers(app.providers)
            }
        }
    }

    let server: URL
    let stage: Stage
    let errorMessage: String?

    var body: some View {
        GeometryReader { proxy in
            ScrollView {
                VStack(spacing: Theme.Spacing.xl) {
                    SignInHeader()
                    SignInCard {
                        content
                    }
                }
                .padding(.horizontal, Theme.Spacing.xl)
                .padding(.vertical, Theme.Spacing.xl * 2)
                .frame(maxWidth: .infinity, minHeight: proxy.size.height)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
        .background(Theme.Colors.bg.ignoresSafeArea())
        #if os(macOS)
        .frame(minWidth: 480, minHeight: 560)
        #endif
    }

    @ViewBuilder
    private var content: some View {
        switch stage {
        case let .authorizing(authorization):
            SignInAuthorizingPanel(authorization: authorization, errorMessage: errorMessage)
                .transition(.opacity)
        case .loadingProviders:
            SignInServerRow(server: server)
            HuddleDivider()
            SignInProviderPanel(providers: nil, errorMessage: errorMessage)
        case let .providers(providers):
            SignInServerRow(server: server)
            HuddleDivider()
            SignInProviderPanel(providers: providers, errorMessage: errorMessage)
        }
    }
}

/// Brand mark, "Sign in to Waddle" and the tagline.
private struct SignInHeader: View {
    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            WaddleBrandMark(size: Theme.Size.signInMark)
                .padding(.bottom, Theme.Spacing.xs)
            Text("Sign in to Waddle")
                .huddleText(Theme.Typography.headline)
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityAddTraits(.isHeader)
            Text("Rooms, discussions, and the people in them.")
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .multilineTextAlignment(.center)
        .accessibilityElement(children: .combine)
    }
}
