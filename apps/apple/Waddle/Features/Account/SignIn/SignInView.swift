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

/// Which frame sign-in follows: the web card on iPad and Mac, Android
/// `login` on iPhone. They differ in their copy as well as their chrome.
enum SignInLayout {
    /// Web `signin-many`: a bordered card.
    case card
    /// Android `login`: the plain column.
    case column

    /// "GitHub" under a "Sign in with" heading on the web card; "Continue
    /// with GitHub" on Android. A lone provider reads "Sign in with GitHub"
    /// on the web too.
    func buttonTitle(for provider: AuthProvider, isOnly: Bool) -> String {
        switch self {
        case .card: return isOnly ? "Sign in with \(provider.title)" : provider.title
        case .column: return "Continue with \(provider.title)"
        }
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
    #if os(iOS)
    @Environment(\.horizontalSizeClass) private var sizeClass
    #endif

    private var layout: SignInLayout {
        #if os(iOS)
        return sizeClass == .compact ? .column : .card
        #else
        return .card
        #endif
    }

    var body: some View {
        GeometryReader { proxy in
            ScrollView {
                VStack(spacing: Theme.Spacing.xl) {
                    SignInHeader(layout: layout)
                    SignInCard(layout: layout) {
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
            providers(nil)
        case let .providers(providers):
            self.providers(providers)
        }
    }

    /// The web card leads with the server; Android leads with the
    /// buttons and keeps the server at the foot.
    @ViewBuilder
    private func providers(_ providers: [AuthProvider]?) -> some View {
        let isReachable = providers?.isEmpty == false
        switch layout {
        case .card:
            SignInServerRow(server: server, isReachable: isReachable)
            HuddleDivider()
            SignInProviderPanel(providers: providers, errorMessage: errorMessage, layout: layout)
        case .column:
            SignInProviderPanel(providers: providers, errorMessage: errorMessage, layout: layout)
            HuddleDivider()
            SignInServerRow(server: server, isReachable: isReachable)
        }
    }
}

/// Brand mark, the title and the tagline: the web's "Sign in to Waddle",
/// or Android's wordmark and "Your community, live."
private struct SignInHeader: View {
    let layout: SignInLayout

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            WaddleBrandMark(size: Theme.Size.signInMark)
                .padding(.bottom, Theme.Spacing.xs)
            switch layout {
            case .card:
                Text("Sign in to Waddle")
                    .huddleText(Theme.Typography.headline)
                    .foregroundStyle(Theme.Colors.fg)
                    .accessibilityAddTraits(.isHeader)
                Text("Rooms, discussions, and the people in them.")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
            case .column:
                SignInWordmark()
                    .accessibilityAddTraits(.isHeader)
                Text("Your community, live. See who's around and drop into a huddle.")
                    .huddleText(Theme.Typography.body)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
        }
        .multilineTextAlignment(.center)
        .accessibilityElement(children: .combine)
    }
}

/// "waddle." with the dot in the live colour (Android `login`).
private struct SignInWordmark: View {
    var body: some View {
        Text("waddle\(Text(".").foregroundStyle(Theme.Colors.live))")
            .foregroundStyle(Theme.Colors.fg)
            .huddleText(Theme.Typography.display)
            .accessibilityLabel(Text("Waddle"))
    }
}
