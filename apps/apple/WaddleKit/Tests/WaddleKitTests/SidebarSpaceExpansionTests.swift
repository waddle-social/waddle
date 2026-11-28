import Foundation
import Testing

@testable import WaddleKit

@MainActor
struct SidebarSpaceExpansionTests {
    @Test func remembersCollapsedSpacesOnThisDeviceForTheSameAccount() throws {
        let suite = "SidebarSpaceExpansionTests.\(UUID())"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let ana = try #require(BareJID(parsing: "ana@waddle.test"))
        let jo = try #require(BareJID(parsing: "jo@waddle.test"))
        let studio = SidebarRoomGroups.Group.ID.space("studio")
        let prefs = SidebarSpaceExpansion(defaults: defaults)

        #expect(prefs.isExpanded(studio, for: ana))
        prefs.setExpanded(false, group: studio, for: ana)
        let restored = SidebarSpaceExpansion(defaults: defaults)
        #expect(!restored.isExpanded(studio, for: ana))
        #expect(restored.isExpanded(studio, for: jo))
        #expect(restored.isExpanded(.standalone, for: ana))
        restored.setExpanded(true, group: studio, for: ana)
        #expect(SidebarSpaceExpansion(defaults: defaults).isExpanded(studio, for: ana))
    }
}
