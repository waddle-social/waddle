import SwiftUI

/// Provider buttons, or loading / failure states while they arrive.
struct SignInProviderPanel: View {
    @Environment(AppState.self) private var app
    @Environment(\.openURL) private var openURL
    @State private var pendingProvider: AuthProvider.ID?

    var body: some View {
        VStack(spacing: Theme.Spacing.m) {
            if app.isLoadingProviders {
                ProgressView("Loading sign-in options")
                    .padding(.vertical, Theme.Spacing.l)
            } else if app.providers.isEmpty {
                SignInProvidersUnavailable(message: app.errorMessage) {
                    Task { await app.retryProviders() }
                }
            } else {
                ForEach(app.providers) { provider in
                    providerButton(provider)
                }
                SignInErrorText(message: app.errorMessage)
            }
        }
    }

    private func providerButton(_ provider: AuthProvider) -> some View {
        Button {
            begin(with: provider)
        } label: {
            HStack(spacing: Theme.Spacing.s) {
                if pendingProvider == provider.id {
                    ProgressView().controlSize(.small)
                }
                Text("Continue with \(provider.title)")
                    .fontWeight(.semibold)
            }
            .frame(maxWidth: .infinity)
        }
        .buttonStyle(.borderedProminent)
        .controlSize(.large)
        .disabled(pendingProvider != nil)
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
        VStack(spacing: Theme.Spacing.m) {
            if let message {
                Label("Can't reach the server", systemImage: "wifi.exclamationmark")
                    .font(.headline)
                Text(message)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .multilineTextAlignment(.center)
            } else {
                Label("No sign-in options", systemImage: "person.crop.circle.badge.questionmark")
                    .font(.headline)
                Text("This server doesn't offer a way to sign in from this app.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            Button("Retry", action: retry)
                .buttonStyle(.bordered)
                .controlSize(.large)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Spacing.m)
    }
}

/// Red footnote for `AppState.errorMessage`.
struct SignInErrorText: View {
    let message: String?

    var body: some View {
        if let message, !message.isEmpty {
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .font(.footnote)
                .foregroundStyle(.red)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
                .accessibilityLabel(Text("Error: \(message)"))
        }
    }
}
