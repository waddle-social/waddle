import SwiftUI
#if os(iOS)
import UIKit
#endif

struct SettingsAppearanceSection: View {
    @Bindable var preferences: Preferences

    var body: some View {
        Section {
            Picker("Appearance", selection: $preferences.appearance) {
                ForEach(Preferences.Appearance.allCases) { appearance in
                    Text(appearance.title).tag(appearance)
                }
            }
            Toggle("Compact messages", isOn: $preferences.compactMessages)
        } header: {
            Text("Appearance")
        } footer: {
            Text("Compact messages use smaller avatars and tighter spacing.")
        }
    }
}

struct SettingsPrivacySection: View {
    @Bindable var preferences: Preferences

    var body: some View {
        Section {
            Toggle("Send read receipts", isOn: $preferences.sendsReadReceipts)
        } header: {
            Text("Privacy")
        } footer: {
            Text("Others see when you've read their direct messages. Your devices still sync read state.")
        }
    }
}

struct SettingsNotificationsSection: View {
    @Bindable var preferences: Preferences
    @Environment(\.openURL) private var openURL

    var body: some View {
        Section {
            Toggle("Show message previews", isOn: $preferences.showsNotificationPreviews)
            Button {
                if let url = Self.systemSettingsURL {
                    openURL(url)
                }
            } label: {
                Label("System notification settings", systemImage: "arrow.up.forward.app")
            }
        } header: {
            Text("Notifications")
        } footer: {
            Text("Choose how each conversation notifies you from its details.")
        }
    }

    private static var systemSettingsURL: URL? {
        #if os(iOS)
        URL(string: UIApplication.openNotificationSettingsURLString)
        #else
        URL(string: "x-apple.systempreferences:com.apple.preference.notifications")
        #endif
    }
}
