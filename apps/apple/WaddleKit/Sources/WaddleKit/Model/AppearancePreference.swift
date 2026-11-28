/// The device's appearance preference. Case order matches Settings, and
/// the raw values are the ones already stored on Apple devices.
public enum AppearancePreference: String, CaseIterable, Identifiable, Sendable {
    case system
    case dark
    case light

    public var id: String { rawValue }

    /// The next choice in the web account menu.
    public var next: Self {
        switch self {
        case .system: .dark
        case .dark: .light
        case .light: .system
        }
    }
}
