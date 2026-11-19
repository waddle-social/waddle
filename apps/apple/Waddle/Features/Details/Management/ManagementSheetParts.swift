import SwiftUI

/// Shown while a management sheet waits for the server.
struct ManagementLoadingView: View {
    let title: String

    var body: some View {
        VStack(spacing: Theme.Spacing.m) {
            ProgressView()
            Text(title)
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
    }
}

/// A sheet whose content could not load, with a retry.
struct ManagementLoadFailedView: View {
    let title: String
    let message: String
    let onRetry: () -> Void

    var body: some View {
        // The empty state's own padding spaces the button.
        VStack(spacing: 0) {
            EmptyStateView(title: title, message: message, symbol: "exclamationmark.triangle")
                .fixedSize(horizontal: false, vertical: true)
            Button("Try again", action: onRetry)
                .buttonStyle(.huddle(.primary))
        }
        .padding(.bottom, Theme.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// An inline failure inside a form.
struct ManagementErrorLabel: View {
    let message: String

    var body: some View {
        Label(message, systemImage: "exclamationmark.triangle")
            .foregroundStyle(Theme.Colors.dangerText)
    }
}

extension View {
    /// Mac sheets size to their content; give them room for a form.
    func managementSheetFrame(minHeight: CGFloat) -> some View {
        #if os(macOS)
        return self.frame(minWidth: 420, idealWidth: 460, minHeight: minHeight)
        #else
        return self
        #endif
    }
}
