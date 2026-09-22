import SwiftUI

/// The server this app signs in to, with an inline editor to change it.
struct SignInServerRow: View {
    @Environment(AppState.self) private var app
    @State private var isEditing = false

    var body: some View {
        VStack(spacing: Theme.Spacing.s) {
            Divider()
            if isEditing {
                SignInServerEditor(initial: app.server.absoluteString) {
                    withAnimation { isEditing = false }
                }
            } else {
                summary
            }
        }
        .padding(.top, Theme.Spacing.s)
    }

    private var summary: some View {
        HStack(spacing: Theme.Spacing.s) {
            Image(systemName: "server.rack")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text("Server")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Text(SignInServerLabel.text(for: app.server))
                    .font(.footnote.weight(.medium))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .accessibilityElement(children: .combine)
            Spacer(minLength: Theme.Spacing.s)
            Button("Change") {
                withAnimation { isEditing = true }
            }
            .controlSize(.small)
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
            Text("Server")
                .font(.caption)
                .foregroundStyle(.secondary)
            field
            HStack {
                Button("Cancel", action: close)
                    .buttonStyle(.borderless)
                Spacer()
                if isApplying {
                    ProgressView().controlSize(.small)
                }
                Button("Use server", action: apply)
                    .buttonStyle(.bordered)
                    .disabled(!isValid || isApplying)
            }
            .controlSize(.small)
        }
    }

    private var field: some View {
        TextField("xmpp.example.com", text: $draft)
            .textFieldStyle(.roundedBorder)
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
