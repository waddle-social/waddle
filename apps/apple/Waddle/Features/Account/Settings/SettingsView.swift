import SwiftUI

/// App settings. On Mac this is the Settings window (tabs, no navigation
/// stack, possibly signed out); on iOS it is pushed from the profile or
/// shown as a sheet inside the caller's navigation stack.
struct SettingsView: View {
    @Environment(AppState.self) private var app

    init() {}

    var body: some View {
        #if os(macOS)
        SettingsTabs(app: app)
        #else
        SettingsForm(app: app)
        #endif
    }
}

#if os(macOS)
/// Mac Settings window: General, Notifications and Account tabs.
private struct SettingsTabs: View {
    let app: AppState

    var body: some View {
        TabView {
            SettingsTabForm {
                SettingsAppearanceSection(preferences: app.preferences)
                SettingsPrivacySection(preferences: app.preferences)
                SettingsAboutSection()
            }
            .tabItem { Label("General", systemImage: "gearshape") }

            SettingsTabForm {
                SettingsNotificationsSection(preferences: app.preferences)
            }
            .tabItem { Label("Notifications", systemImage: "bell.badge") }

            SettingsTabForm {
                SettingsAccountSection(app: app)
            }
            .tabItem { Label("Account", systemImage: "person.crop.circle") }
        }
    }
}

/// A grouped form sized for the Settings window.
private struct SettingsTabForm<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        Form { content }
            .formStyle(.grouped)
            .frame(minHeight: 320)
    }
}
#else
/// iOS settings list.
private struct SettingsForm: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.isPresented) private var isPresented
    let app: AppState

    var body: some View {
        Form {
            SettingsAppearanceSection(preferences: app.preferences)
            SettingsNotificationsSection(preferences: app.preferences)
            SettingsPrivacySection(preferences: app.preferences)
            SettingsAccountSection(app: app)
            SettingsAboutSection()
        }
        .formStyle(.grouped)
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if showsDoneButton {
                    Button("Done") { dismiss() }
                }
            }
        }
    }

    /// Only the settings sheet needs Done; when pushed, Back is enough.
    private var showsDoneButton: Bool {
        isPresented && app.session?.navigation.sheet == SheetRoute.settings
    }
}
#endif
