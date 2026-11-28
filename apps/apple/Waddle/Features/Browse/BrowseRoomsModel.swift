import Observation
import WaddleKit

/// Opens rooms from Browse rooms and handles membership changes for rooms the account left.
@MainActor
@Observable
final class BrowseRoomsModel {
    private(set) var joining: BareJID?
    private(set) var failure: String?
    @ObservationIgnored private var shouldOpenAfterJoin = false

    func pageDidDisappear() {
        shouldOpenAfterJoin = false
    }

    func open(_ room: BareJID, using session: SessionCoordinator, navigation: NavigationModel) async {
        guard joining == nil, let channel = session.directory.channel(for: room), !channel.isGroupDM else { return }
        if !channel.hasLeft {
            // The conversation screen joins discovered rooms on demand.
            navigation.open(channel.conversation, returningTo: .browseRooms)
            return
        }

        joining = room
        failure = nil
        shouldOpenAfterJoin = true
        defer { joining = nil }
        do {
            try await session.rejoinRoom(room)
            // If the user moved on during the join, leave their new screen alone.
            guard shouldOpenAfterJoin, navigation.sheet == nil,
                navigation.tab == .home, navigation.homePath.last == .browseRooms,
                navigation.splitSection == .browseRooms, navigation.selection == nil,
                let rejoined = session.directory.channel(for: room), !rejoined.hasLeft
            else { return }
            navigation.open(rejoined.conversation, returningTo: .browseRooms)
        } catch RoomRejoinError.roomIsGone {
            failure = "That room no longer exists."
        } catch {
            failure = ActionErrorCopy.message(for: error, fallback: "Couldn't join the room. Try again.")
        }
    }
}
