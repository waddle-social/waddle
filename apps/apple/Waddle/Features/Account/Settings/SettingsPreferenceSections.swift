import SwiftUI
import WaddleKit
#if os(iOS)
import UIKit
#endif

/// System, Night or Daylight as preview tiles (web `settings`
/// Appearance), and compact messages.
struct SettingsAppearanceSection: View {
    @Bindable var preferences: Preferences

    var body: some View {
        Section {
            SettingsTilePicker(
                selection: $preferences.appearance,
                options: Preferences.Appearance.settingsOrder.map {
                    .init(value: $0, title: $0.title, systemImage: $0.symbol)
                },
                label: "Appearance"
            ) { appearance in
                AppearancePreview(appearance: appearance)
            }
            FormToggle("Compact messages", isOn: $preferences.compactMessages)
        } header: {
            FormSectionHeader("Appearance")
        } footer: {
            FormSectionFooter("Compact messages use smaller avatars and tighter spacing.")
        }
    }
}

struct SettingsPrivacySection: View {
    @Bindable var preferences: Preferences

    var body: some View {
        Section {
            FormToggle("Send read receipts", isOn: $preferences.sendsReadReceipts)
            FormToggle("Send typing indicators", isOn: $preferences.sendsChatStates)
        } header: {
            FormSectionHeader("Privacy")
        } footer: {
            FormSectionFooter("Others see when you've read their direct messages. Your devices still sync read state. Others see when you're typing.")
        }
    }
}

struct SettingsNotificationsSection: View {
    @Bindable var preferences: Preferences
    let pushRegistrationStatus: AppState.PushRegistrationStatus
    let notificationPermission: NotificationControl.Permission
    @Environment(\.openURL) private var openURL

    var body: some View {
        Section {
            FormToggle("Show message previews", isOn: $preferences.showsNotificationPreviews)
            FormToggle("Notifications", isOn: $preferences.notificationsEnabled)
            if preferences.notificationsEnabled {
                FormValueRow(title: "Push registration", value: pushRegistrationStatus.title)
                if case let .failed(message) = pushRegistrationStatus {
                    Text(message)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.dangerText)
                }
            }
            if notificationPermission == .denied {
                Label("Notifications are turned off in system settings.", systemImage: "bell.slash")
                    .huddleText(Theme.Typography.caption)
                    .foregroundStyle(Theme.Colors.warningText)
            }
            FormActionButton(title: "System notification settings", symbol: "arrow.up.forward.app") {
                if let url = Self.systemSettingsURL {
                    openURL(url)
                }
            }
        } header: {
            FormSectionHeader("Notifications")
        } footer: {
            FormSectionFooter(footer)
        }
    }

    private var footer: String {
        let conversations = "Choose how each conversation notifies you, and whether its notifications show messages, from its details."
        return preferences.notificationsEnabled
            ? "\(conversations) Push notifications reach this device while Waddle is closed, if the server has them set up."
            : "\(conversations) Message alerts and sounds are off on this device. Calls can still ring while Waddle is open."
    }

    private static var systemSettingsURL: URL? {
        #if os(iOS)
        URL(string: UIApplication.openNotificationSettingsURLString)
        #else
        URL(string: "x-apple.systempreferences:com.apple.preference.notifications")
        #endif
    }
}

/// A miniature of the app in one appearance (web `settings` Appearance
/// swatch): the ground, a muted bar and the accent.
private struct AppearancePreview: View {
    let appearance: Preferences.Appearance

    var body: some View {
        switch appearance.colorScheme {
        case let .some(scheme):
            swatch.environment(\.colorScheme, scheme)
        case .none:
            // System: Daylight on the left, Night on the right.
            HStack(spacing: 0) {
                swatch.environment(\.colorScheme, .light)
                swatch.environment(\.colorScheme, .dark)
            }
        }
    }

    private var swatch: some View {
        ZStack(alignment: .bottom) {
            Theme.Colors.bg
            HStack(spacing: Theme.Spacing.xs) {
                Capsule().fill(Theme.Colors.surface2)
                Capsule().fill(Theme.Colors.action)
            }
            .frame(height: Theme.Size.previewBar)
            .padding(Theme.Spacing.s)
        }
        .accessibilityHidden(true)
    }
}
