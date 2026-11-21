import Foundation

/// Who may pin messages in a room.
public enum PinPermission: Hashable, Sendable {
    case adminsOnly
    case anyone
}

/// Editable room fields. A nil name or summary is one the room does not
/// have; a nil pin permission is a setting the server does not offer.
public struct RoomSettings: Hashable, Sendable {
    public let name: String?
    public let summary: String?
    public let pinPermission: PinPermission?

    public init(name: String? = nil, summary: String? = nil, pinPermission: PinPermission? = nil) {
        self.name = name
        self.summary = summary
        self.pinPermission = pinPermission
    }
}

/// Room settings to change. Nil keeps the current value; an empty summary clears it.
public struct RoomSettingsPatch: Hashable, Sendable {
    public let name: String?
    public let summary: String?
    public let pinPermission: PinPermission?

    public init(name: String? = nil, summary: String? = nil, pinPermission: PinPermission? = nil) {
        self.name = name
        self.summary = summary
        self.pinPermission = pinPermission
    }

    /// Whether every field keeps its current value.
    public var isEmpty: Bool { name == nil && summary == nil && pinPermission == nil }

    /// Trims the draft and keeps only what differs. An empty name is never
    /// sent; an emptied summary clears it.
    public static func changes(from settings: RoomSettings, name: String, summary: String, pinPermission: PinPermission?) -> RoomSettingsPatch {
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let summary = summary.trimmingCharacters(in: .whitespacesAndNewlines)
        return RoomSettingsPatch(
            name: !name.isEmpty && name != settings.name ? name : nil,
            summary: summary != (settings.summary ?? "") ? summary : nil,
            pinPermission: settings.pinPermission != nil && pinPermission != settings.pinPermission ? pinPermission : nil
        )
    }
}

/// The message history available to a group DM invitee.
public enum GroupDMHistoryAccess: Hashable, Sendable {
    case fromJoin
    case full
}
