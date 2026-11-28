import Foundation
import Observation

/// The device's choice for in-app message sounds.
@MainActor
@Observable
public final class MessageSoundPreference {
    private static let key = "waddle.apple.message-sounds-enabled"
    private let defaults: UserDefaults

    public var isEnabled: Bool {
        didSet { defaults.set(isEnabled, forKey: Self.key) }
    }

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        isEnabled = defaults.object(forKey: Self.key) as? Bool ?? true
    }
}
