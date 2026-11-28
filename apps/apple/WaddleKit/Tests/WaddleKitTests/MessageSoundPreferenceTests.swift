import Foundation
import Testing

@testable import WaddleKit

@MainActor
struct MessageSoundPreferenceTests {
    @Test func soundsAreOnUntilTurnedOff() throws {
        let suite = "message-sound-preference-\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }

        let preference = MessageSoundPreference(defaults: defaults)
        #expect(preference.isEnabled)

        preference.isEnabled = false
        #expect(!MessageSoundPreference(defaults: defaults).isEnabled)

        preference.isEnabled = true
        #expect(MessageSoundPreference(defaults: defaults).isEnabled)
    }
}
