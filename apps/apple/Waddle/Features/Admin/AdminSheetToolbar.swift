import SwiftUI

/// The native sheet actions used by room, space and person sheets.
struct AdminSheetToolbar: ToolbarContent {
    let dismissTitle: String
    let confirmTitle: String
    var canDismiss = true
    let canConfirm: Bool
    var isWorking = false
    let onDismiss: () -> Void
    let onConfirm: () -> Void

    var body: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button(dismissTitle, action: onDismiss)
                .disabled(!canDismiss)
        }
        ToolbarItem(placement: .confirmationAction) {
            if isWorking {
                ProgressView()
                    .controlSize(.small)
                    .tint(Theme.Colors.action)
                    .accessibilityLabel(Text("Working"))
            } else {
                Button(confirmTitle, action: onConfirm)
                    .disabled(!canConfirm)
            }
        }
    }
}
