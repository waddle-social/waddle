import SwiftUI
import WaddleKit

/// Back / Next / Complete for a command stage; the last one is primary.
struct ExtensionCommandActionBar: View {
    let actions: [ExtensionCommandAction]
    let isBusy: Bool
    let onAction: (ExtensionCommandAction) -> Void

    var body: some View {
        if !actions.isEmpty {
            HStack(spacing: Theme.Spacing.s) {
                if isBusy {
                    ProgressView()
                        .controlSize(.small)
                }
                Spacer(minLength: 0)
                ForEach(actions, id: \.self) { action in
                    button(for: action)
                }
            }
            .padding(.horizontal, Theme.Spacing.l)
            .padding(.vertical, Theme.Spacing.m)
            .background(.bar)
            .disabled(isBusy)
        }
    }

    @ViewBuilder
    private func button(for action: ExtensionCommandAction) -> some View {
        let title = ExtensionCommandCopy.title(for: action)
        if action == actions.last {
            Button(title) { onAction(action) }
                .buttonStyle(.borderedProminent)
                .keyboardShortcut(.defaultAction)
        } else {
            Button(title) { onAction(action) }
                .buttonStyle(.bordered)
        }
    }
}
