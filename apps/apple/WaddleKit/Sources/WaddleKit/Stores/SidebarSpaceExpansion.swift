import Foundation
import Observation

/// Remembers which room groups each account collapsed on this device.
@MainActor
@Observable
public final class SidebarSpaceExpansion {
    /// The windows in this process share the device's preferences.
    public static let device = SidebarSpaceExpansion()

    @ObservationIgnored private let defaults: UserDefaults
    private var collapsed: [String: [String]]
    private static let key = "sidebar.collapsedRoomGroups"

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        collapsed = defaults.dictionary(forKey: Self.key) as? [String: [String]] ?? [:]
    }

    public func isExpanded(_ group: SidebarRoomGroups.Group.ID, for account: BareJID) -> Bool {
        collapsed[account.description]?.contains(key(for: group)) != true
    }

    public func setExpanded(_ expanded: Bool, group: SidebarRoomGroups.Group.ID, for account: BareJID) {
        guard isExpanded(group, for: account) != expanded else { return }
        let accountKey = account.description
        var groups = Set(collapsed[accountKey] ?? [])
        if expanded {
            groups.remove(key(for: group))
        } else {
            groups.insert(key(for: group))
        }
        collapsed[accountKey] = groups.isEmpty ? nil : groups.sorted()
        defaults.set(collapsed, forKey: Self.key)
    }

    private func key(for group: SidebarRoomGroups.Group.ID) -> String {
        switch group {
        case let .space(id): "space:\(id)"
        case .standalone: "standalone"
        }
    }
}
