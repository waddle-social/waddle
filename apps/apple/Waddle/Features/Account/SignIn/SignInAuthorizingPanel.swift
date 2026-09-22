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

    var body: some View {
        VStack(spacing: Theme.Spacing.l) {
            Text("Finish signing in")
                .font(.title2.weight(.semibold))
            SignInUserCode(code: authorization.userCode)
            Text("Approve this sign-in in your browser. This page updates automatically.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            HStack(spacing: Theme.Spacing.s) {
                ProgressView().controlSize(.small)
                Text("Waiting for approval")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
            actions
            SignInErrorText(message: app.errorMessage)
        }
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
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            Button("Cancel", role: .cancel) {
                app.cancelSignIn()
            }
            .buttonStyle(.borderless)
            .controlSize(.large)
        }
    }
}

/// The large, copyable device code.
private struct SignInUserCode: View {
    let code: String
    @State private var copyCount = 0

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            Text("Your code")
                .font(.footnote.weight(.medium))
                .foregroundStyle(.secondary)
            Text(code)
                .font(.system(.largeTitle, design: .monospaced).weight(.semibold))
                .kerning(2)
                .textSelection(.enabled)
                .lineLimit(1)
                .minimumScaleFactor(0.5)
                .accessibilityLabel(Text("Code \(code)"))
            Button {
                copy()
            } label: {
                Label(copyCount > 0 ? "Copied" : "Copy code", systemImage: copyCount > 0 ? "checkmark" : "doc.on.doc")
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .sensoryFeedback(.success, trigger: copyCount)
        }
        .padding(Theme.Spacing.l)
        .frame(maxWidth: .infinity)
        .background(
            Color.secondary.opacity(0.1),
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
