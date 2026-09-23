import Foundation

enum SettingsAppVersion {
    /// "1.2 (45)" from an Info.plist dictionary; the build is omitted when
    /// missing or equal to the version.
    static func label(from info: [String: Any]?) -> String {
        let version = info?["CFBundleShortVersionString"] as? String
        let build = info?["CFBundleVersion"] as? String
        switch (version, build) {
        case let (version?, build?) where build != version:
            return "\(version) (\(build))"
        case let (version?, _):
            return version
        case let (nil, build?):
            return build
        case (nil, nil):
            return "Unknown"
        }
    }

    static var current: String {
        label(from: Bundle.main.infoDictionary)
    }
}
