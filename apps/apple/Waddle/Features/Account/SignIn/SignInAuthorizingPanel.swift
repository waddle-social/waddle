import SwiftUI
#if os(iOS)
import UIKit
#elseif os(macOS)
import AppKit
#endif

/// RFC 8628 device flow in progress: show the user code and wait for the
/// browser approval that `AppState` polls for.
struct SignInAuthorizingPanel: View {
    @Environment(AppState.self) private var app
    @Environment(\.openURL) private var openURL
    let authorization: DeviceAuthorization
    let errorMessage: String?

    var body: some View {
        VStack(spacing: Theme.Spacing.l) {
            Text("Finish signing in")
                .huddleText(Theme.Typography.title)
                .foregroundStyle(Theme.Colors.fg)
            SignInUserCode(code: authorization.userCode)
            Text("Approve this sign-in in your browser. This page updates on its own.")
                .huddleText(Theme.Typography.body)
                .foregroundStyle(Theme.Colors.fgMuted)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: Theme.Spacing.s) {
                ProgressView().controlSize(.small)
                Text("Waiting for approval")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.fgMuted)
            }
            .accessibilityElement(children: .combine)
            actions
            SignInErrorText(message: errorMessage)
        }
        .frame(maxWidth: .infinity)
    }

    private var actions: some View {
        VStack(spacing: Theme.Spacing.s) {
            Button {
                if let url = app.verificationURL() {
                    openURL(url)
                }
            } label: {
                Label("Open browser again", systemImage: "safari")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.huddle(.primary, size: .large))

            Button("Cancel", role: .cancel) {
                app.cancelSignIn()
            }
            .buttonStyle(.huddle(size: .large))
        }
    }
}

/// The large, copyable device code.
private struct SignInUserCode: View {
    let code: String
    @State private var copyCount = 0

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            FormSectionHeader("Your code")
            Text(code)
                .huddleText(Theme.Typography.signInCode)
                .foregroundStyle(Theme.Colors.fg)
                .textSelection(.enabled)
                .lineLimit(1)
                .minimumScaleFactor(0.5)
                .accessibilityLabel(Text("Code \(code)"))
            Button {
                copy()
            } label: {
                Label(copyCount > 0 ? "Copied" : "Copy code", systemImage: copyCount > 0 ? "checkmark" : "doc.on.doc")
            }
            .buttonStyle(.huddle(size: .small))
            .sensoryFeedback(.success, trigger: copyCount)
        }
        .padding(Theme.Spacing.l)
        .frame(maxWidth: .infinity)
        .background(
            Theme.Colors.surface2,
            in: RoundedRectangle(cornerRadius: Theme.Radius.large, style: .continuous)
        )
    }

    private func copy() {
        #if os(iOS)
        UIPasteboard.general.string = code
        #elseif os(macOS)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(code, forType: .string)
        #endif
        copyCount += 1
    }
}
