import SwiftUI
import WaddleKit

/// Confirmations and prompts the message actions need: delete, moderator
/// removal with a reason, the reaction picker sheet, and failures.
struct MessageActionDialogs: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Bindable var actions: MessageActionModel

    func body(content: Content) -> some View {
        content
            .confirmationDialog("Delete this message?", isPresented: deletionBinding, titleVisibility: .visible) {
                Button("Delete", role: .destructive) {
                    guard let item = actions.pendingDeletion else { return }
                    Task { await actions.delete(item, session: session) }
                }
                Button("Cancel", role: .cancel) {
                    actions.pendingDeletion = nil
                }
            } message: {
                Text("It will be removed for everyone.")
            }
            .alert("Remove this message?", isPresented: removalBinding) {
                TextField("Reason (optional)", text: $actions.removalReason)
                Button("Remove", role: .destructive) {
                    guard let item = actions.pendingRemoval else { return }
                    let reason = actions.removalReason
                    Task { await actions.remove(item, reason: reason, session: session) }
                }
                Button("Cancel", role: .cancel) {
                    actions.pendingRemoval = nil
                }
            } message: {
                Text("The message will be removed for everyone in the channel.")
            }
            .sheet(item: $actions.reactionTarget) { item in
                MessageReactionPickerSheet(item: item, actions: actions)
                    .environment(session)
            }
            .background {
                Color.clear
                    .alert("Something went wrong", isPresented: errorBinding) {
                        Button("OK", role: .cancel) {
                            actions.errorMessage = nil
                        }
                    } message: {
                        Text(actions.errorMessage ?? "")
                    }
            }
    }

    private var deletionBinding: Binding<Bool> {
        Binding(
            get: { actions.pendingDeletion != nil },
            set: { if !$0 { actions.pendingDeletion = nil } }
        )
    }

    private var removalBinding: Binding<Bool> {
        Binding(
            get: { actions.pendingRemoval != nil },
            set: { if !$0 { actions.pendingRemoval = nil } }
        )
    }

    private var errorBinding: Binding<Bool> {
        Binding(
            get: { actions.errorMessage != nil },
            set: { if !$0 { actions.errorMessage = nil } }
        )
    }
}
