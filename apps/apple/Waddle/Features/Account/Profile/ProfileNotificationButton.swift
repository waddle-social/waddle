import SwiftUI
import WaddleKit

/// The quick bell in the account panel.
struct ProfileNotificationButton: View {
    @Environment(AppState.self) private var app

    var body: some View {
        let control = NotificationControl(
            permission: app.notificationPermission,
            notificationsEnabled: app.preferences.notificationsEnabled
        )
        FormActionButton(title: title(for: control), symbol: symbol(for: control)) {
            if let action = control.action {
                app.performNotificationAction(action)
            }
        }
        .disabled(control.action == nil)
        .opacity(control.action == nil ? 0.5 : 1)
    }

    private func title(for control: NotificationControl) -> String {
        switch control {
        case .enable: "Enable notifications"
        case .blocked: "Notifications blocked — update system settings"
        case .enabled: "Notifications enabled"
        case .disabled: "Notifications disabled"
        }
    }

    private func symbol(for control: NotificationControl) -> String {
        switch control {
        case .enable, .enabled: "bell"
        case .blocked, .disabled: "bell.slash"
        }
    }
}
