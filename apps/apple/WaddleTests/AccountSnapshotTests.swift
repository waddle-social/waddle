import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the account screens: sign-in (Web
/// `signin-many`), its device code step, Settings and the profile. Forms
/// only draw in a real window, so these are screen snapshots (see
/// `assertScreenSnapshot`).
@MainActor
final class AccountSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let screen = CGSize.phone
    private static let settings = CGSize(width: 393, height: 900)
    #else
    private static let screen = CGSize(width: 760, height: 720)
    /// The Settings window's General tab.
    private static let settings = CGSize(width: 520, height: 760)
    #endif

    /// The sample account's own server.
    private static let server: URL = {
        var components = URLComponents()
        components.scheme = "https"
        components.host = SampleSession.ana.jid.domain
        return components.url!
    }()

    func testSignInWithThreeProviders() async throws {
        let providers = [
            AuthProvider(id: "github", kind: "oauth", displayName: "GitHub"),
            AuthProvider(id: "codeberg", kind: "oauth", displayName: "Codeberg"),
            AuthProvider(id: "google", kind: "oidc", displayName: "Google"),
        ]
        let view = SignInScreen(server: Self.server, stage: .providers(providers), errorMessage: nil)
        try await assertScreenSnapshot(of: SampleSession().host(view), size: Self.screen)
    }

    func testSignInWaitingForTheBrowser() async throws {
        let json = #"{"device_code":"device","user_code":"WDDL-4271","verification_uri":"https://waddle.test/device","interval":5}"#
        let authorization = try JSONDecoder().decode(DeviceAuthorization.self, from: Data(json.utf8))
        let view = SignInScreen(server: Self.server, stage: .authorizing(authorization), errorMessage: nil)
        try await assertScreenSnapshot(of: SampleSession().host(view), size: Self.screen)
    }

    func testSettingsGeneral() async throws {
        let suite = "account-snapshots"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }
        let preferences = Preferences(defaults: defaults)
        preferences.appearance = .light
        let view = Form {
            SettingsAppearanceSection(preferences: preferences)
            SettingsMessagesSection(preferences: preferences)
            SettingsPrivacySection(preferences: preferences)
        }
        .huddleForm()
        try await assertScreenSnapshot(of: view, size: Self.settings)
    }

    func testProfile() async throws {
        let sample = SampleSession()
        sample.coordinator.status.statusText = "Reviewing the identity drafts"
        let view = NavigationStack { ProfileView() }
        try await assertScreenSnapshot(of: sample.host(view), size: Self.settings)
    }
}
