import SwiftUI

/// "Sign in with" and a button per provider, the first the primary one;
/// or loading and failure states while they arrive.
struct SignInProviderPanel: View {
    @Environment(AppState.self) private var app
    @Environment(\.openURL) private var openURL
    @State private var pendingProvider: AuthProvider.ID?
    /// Nil while they load.
    let providers: [AuthProvider]?
    let errorMessage: String?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.m) {
            if let providers, !providers.isEmpty {
                FormSectionHeader("Sign in with")
                ForEach(providers) { provider in
                    providerButton(provider, isPrimary: provider.id == providers.first?.id)
                }
                SignInErrorText(message: errorMessage)
            } else if providers == nil {
                HStack(spacing: Theme.Spacing.s) {
                    ProgressView().controlSize(.small)
                    Text("Loading sign-in options")
                        .huddleText(Theme.Typography.body)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, Theme.Spacing.l)
            } else {
                SignInProvidersUnavailable(message: errorMessage) {
                    Task { await app.retryProviders() }
                }
            }
        }
    }

    private func providerButton(_ provider: AuthProvider, isPrimary: Bool) -> some View {
        Button {
            begin(with: provider)
        } label: {
            HStack(spacing: Theme.Spacing.s) {
                if pendingProvider == provider.id {
                    ProgressView().controlSize(.small)
                }
                Text(provider.title)
            }
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.huddle(isPrimary ? .primary : .secondary, size: .large))
        .disabled(pendingProvider != nil)
        .accessibilityLabel(Text("Sign in with \(provider.title)"))
    }

    private func begin(with provider: AuthProvider) {
        pendingProvider = provider.id
        Task {
            if let url = await app.beginSignIn(with: provider) {
                openURL(url)
            }
            pendingProvider = nil
        }
    }
}

/// Shown when the provider list could not be loaded.
private struct SignInProvidersUnavailable: View {
    let message: String?
    let retry: () -> Void

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            if let message {
                Text("Can't reach the server")
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fg)
                Text(message)
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.dangerText)
            } else {
                Text("No sign-in options")
                    .huddleText(Theme.Typography.subtitle)
                    .foregroundStyle(Theme.Colors.fg)
                Text("This server doesn't offer a way to sign in from this app. Try another server.")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            Button("Try again", action: retry)
                .buttonStyle(.huddle())
                .padding(.top, Theme.Spacing.xs)
        }
        .multilineTextAlignment(.center)
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.m)
    }
}

/// A sign-in failure, in danger text.
struct SignInErrorText: View {
    let message: String?

    var body: some View {
        if let message, !message.isEmpty {
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .huddleText(Theme.Typography.caption)
                .foregroundStyle(Theme.Colors.dangerText)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .accessibilityLabel(Text("Error: \(message)"))
        }
    }
}
