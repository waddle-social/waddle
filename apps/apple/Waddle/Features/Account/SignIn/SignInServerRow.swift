import SwiftUI

/// The server this app signs in to (web `signin-many` "Homeserver"), with
/// an inline editor to change it.
struct SignInServerRow: View {
    let server: URL
    /// The server answered with a way to sign in.
    let isReachable: Bool
    @State private var isEditing = false

    var body: some View {
        if isEditing {
            SignInServerEditor(initial: server.absoluteString) {
                withAnimation { isEditing = false }
            }
        } else {
            summary
        }
    }

    private var summary: some View {
        HStack(alignment: .top, spacing: Theme.Spacing.s) {
            VStack(alignment: .leading, spacing: Theme.Spacing.s) {
                FormSectionHeader("Homeserver")
                HStack(spacing: Theme.Spacing.s) {
                    Circle()
                        .fill(isReachable ? Theme.Colors.live : Theme.Colors.fgSoft)
                        .frame(width: Theme.Size.liveDot, height: Theme.Size.liveDot)
                        .accessibilityHidden(true)
                    Text(SignInServerLabel.text(for: server))
                        .huddleText(Theme.Typography.code)
                        .foregroundStyle(Theme.Colors.fg)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: Theme.Spacing.s)
            Button {
                withAnimation { isEditing = true }
            } label: {
                Label("Edit", systemImage: "chevron.down")
                    .labelStyle(.trailingIcon)
            }
            .buttonStyle(.huddle(size: .small))
            .accessibilityLabel(Text("Edit homeserver"))
            .accessibilityHint(Text("Sign in to a different Waddle server"))
        }
    }
}

/// Server URL field plus apply / cancel.
private struct SignInServerEditor: View {
    @Environment(AppState.self) private var app
    @State private var draft: String
    @State private var isApplying = false
    let close: () -> Void

    init(initial: String, close: @escaping () -> Void) {
        _draft = State(initialValue: initial)
        self.close = close
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            FormSectionHeader("Homeserver")
            field
            HStack(spacing: Theme.Spacing.s) {
                Spacer()
                if isApplying {
                    ProgressView().controlSize(.small)
                }
                Button("Cancel", action: close)
                    .buttonStyle(.huddle(size: .small))
                Button("Use server", action: apply)
                    .buttonStyle(.huddle(.primary, size: .small))
                    .disabled(!isValid || isApplying)
            }
        }
    }

    private var field: some View {
        TextField("waddle.example.com", text: $draft)
            .huddleField()
            .autocorrectionDisabled()
            #if os(iOS)
            .keyboardType(.URL)
            .textContentType(.URL)
            .textInputAutocapitalization(.never)
            #endif
            .submitLabel(.go)
            .onSubmit(apply)
            .accessibilityLabel(Text("Server address"))
    }

    private var isValid: Bool {
        ServerSettings.normalized(from: draft) != nil
    }

    private func apply() {
        guard isValid, !isApplying else { return }
        isApplying = true
        Task {
            await app.changeServer(to: draft)
            isApplying = false
            close()
        }
    }
}
