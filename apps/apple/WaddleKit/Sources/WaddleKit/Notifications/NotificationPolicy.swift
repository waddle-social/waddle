/// Which alerts and sounds Waddle presents for its current settings.
public struct NotificationPolicy: Sendable {
    public enum Kind: Sendable {
        case message
        case incomingCall
        case missedCall
    }

    public let showAlert: Bool
    public let playSound: Bool

    public init(
        kind: Kind,
        isDoNotDisturb: Bool,
        notificationsEnabled: Bool = true,
        messageSoundsEnabled: Bool = true
    ) {
        switch kind {
        case .message:
            showAlert = notificationsEnabled && !isDoNotDisturb
            playSound = showAlert && messageSoundsEnabled
        case .incomingCall:
            // Like web, the live ringtone and its banner follow presence.
            showAlert = !isDoNotDisturb
            playSound = showAlert
        case .missedCall:
            // Missed calls are delivered as push notifications, like on web.
            showAlert = notificationsEnabled
            playSound = showAlert
        }
    }
}
