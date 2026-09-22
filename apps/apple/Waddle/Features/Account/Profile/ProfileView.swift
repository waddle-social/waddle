import SwiftUI
import WaddleKit

/// The signed-in user's profile: avatar, availability, status message,
/// XEP-0107 mood, settings and sign out.
///
/// iPhone shows it as the "You" tab; iPad and Mac present it as a sheet.
struct ProfileView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @Environment(\.isPresented) private var isPresented

    init() {}

    var body: some View {
        Form {
            Section {
                ProfileHeader()
                ProfileAvatarControls()
            }
            ProfileStatusSection()
            Section("Mood") {
                ProfileMoodRow()
            }
            Section {
                ProfileSettingsLink()
            }
            Section {
                AccountSignOutButton()
            }
        }
        .formStyle(.grouped)
        .navigationTitle("Profile")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                if showsDoneButton {
                    Button("Done") { dismiss() }
                }
            }
        }
        .task { await session.loadOwnMood() }
        #if os(macOS)
        .frame(minWidth: 440, idealWidth: 480, minHeight: 560, idealHeight: 640)
        #endif
    }

    /// Only the sheet presentation needs a way out; the phone tab does not.
    private var showsDoneButton: Bool {
        isPresented && navigation.sheet == SheetRoute.profile
    }
}

/// Opens settings: pushed on iOS, the Settings window on Mac.
private struct ProfileSettingsLink: View {
    var body: some View {
        #if os(macOS)
        SettingsLink {
            Label("Settings…", systemImage: "gearshape")
        }
        #else
        NavigationLink {
            SettingsView()
        } label: {
            Label("Settings", systemImage: "gearshape")
        }
        #endif
    }
}
