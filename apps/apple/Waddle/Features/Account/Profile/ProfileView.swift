import SwiftUI
import WaddleKit

/// The signed-in user's profile: avatar, details, status, what they share
/// (mood, activity, tune), settings and sign out. In the "You" tab a
/// community owner also finds the admin console here.
///
/// iPhone shows it as the "You" tab; iPad and Mac present it as a sheet.
struct ProfileView: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    @Environment(\.dismiss) private var dismiss
    @Environment(\.isPresented) private var isPresented

    /// The stack this sits in can push the admin console (the "You" tab).
    var opensAdmin = false

    var body: some View {
        Form {
            Section {
                ProfileHeader()
                ProfileAvatarControls()
            }
            Section {
                ProfileDetailsLink()
            }
            ProfileStatusSection()
            ProfileSharingSection()
            Section {
                ProfileSettingsLink()
                if opensAdmin, session.isCommunityOwner {
                    NavigationLink(value: YouRoute.admin) {
                        FormRowLabel(title: "Admin", symbol: "shield")
                    }
                }
            }
            Section {
                AccountSignOutButton()
            }
        }
        .huddleForm()
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
        .task { await session.loadOwnSharedStatus() }
        .task { _ = try? await session.loadOwnDetails() }
        #if os(macOS)
        .frame(minWidth: 440, idealWidth: 480, minHeight: 560, idealHeight: 640)
        #endif
    }

    /// Only the sheet presentation needs a way out; the phone tab does not.
    private var showsDoneButton: Bool {
        isPresented && navigation.sheet == SheetRoute.profile
    }
}

/// Opens the details editor, with the name and pronouns as they stand.
private struct ProfileDetailsLink: View {
    @Environment(SessionCoordinator.self) private var session

    var body: some View {
        NavigationLink {
            ProfileDetailsEditor()
        } label: {
            FormRowLabel(title: "Your details", symbol: "person.text.rectangle") {
                Text(summary)
                    .lineLimit(1)
            }
        }
    }

    private var summary: String {
        guard let details = session.status.details else { return "" }
        return [details.fullName, details.pronouns].filter { !$0.isEmpty }.joined(separator: " · ")
    }
}

/// Opens settings: pushed on iOS, the Settings window on Mac.
private struct ProfileSettingsLink: View {
    var body: some View {
        #if os(macOS)
        SettingsLink {
            FormRowLabel(title: "Settings…", symbol: "gearshape")
        }
        .buttonStyle(.plain)
        #else
        NavigationLink {
            SettingsView()
        } label: {
            FormRowLabel(title: "Settings", symbol: "gearshape")
        }
        #endif
    }
}
