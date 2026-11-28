/// The account bell combines system permission with this device's preference.
public enum NotificationControl: Sendable {
    public enum Permission: Sendable {
        case undetermined
        case denied
        case granted
    }

    public enum Action: Equatable, Sendable {
        case requestPermission
        case setEnabled(Bool)
    }

    case enable
    case blocked
    case enabled
    case disabled

    public init(permission: Permission, notificationsEnabled: Bool) {
        switch permission {
        case .undetermined: self = .enable
        case .denied: self = .blocked
        case .granted: self = notificationsEnabled ? .enabled : .disabled
        }
    }

    public var action: Action? {
        switch self {
        case .enable: .requestPermission
        case .blocked: nil
        case .enabled: .setEnabled(false)
        case .disabled: .setEnabled(true)
        }
    }
}
