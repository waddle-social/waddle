import SwiftUI
import WaddleKit

/// Edit, rename and add people: the actions that change a room or group
/// chat and keep it. Nothing shows when none are allowed.
struct RoomManagementSection: View {
    let actions: [RoomManagementAction]
    let onSelect: (RoomManagementAction) -> Void

    var body: some View {
        let shown = actions.filter { !$0.isDestructive }
        if !shown.isEmpty {
            Section {
                ForEach(shown, id: \.self) { action in
                    Button {
                        onSelect(action)
                    } label: {
                        Label(action.title, systemImage: action.symbol)
                    }
                }
            }
        }
    }
}

/// Delete or leave, at the foot of the details, apart from everything
/// else. Each asks for confirmation first.
struct RoomExitSection: View {
    let actions: [RoomManagementAction]
    /// The delete or leave in flight, if any.
    let exiting: RoomManagementAction?
    let onSelect: (RoomManagementAction) -> Void

    var body: some View {
        let shown = actions.filter(\.isDestructive)
        if !shown.isEmpty {
            Section {
                ForEach(shown, id: \.self) { action in
                    Button(role: .destructive) {
                        onSelect(action)
                    } label: {
                        HStack(spacing: Theme.Spacing.s) {
                            Label(action.title, systemImage: action.symbol)
                                // The role alone leaves a Mac push button grey.
                                .foregroundStyle(Theme.Colors.dangerText)
                            if exiting == action {
                                Spacer(minLength: 0)
                                ProgressView()
                                    .controlSize(.small)
                            }
                        }
                    }
                    .disabled(exiting != nil)
                }
            }
        }
    }
}
