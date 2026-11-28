import Foundation
import Observation
import WaddleKit

/// A screen pushed inside a navigation stack.
enum Route: Hashable {
    case conversation(ConversationID)
    case thread(ConversationID, rootID: String)
    case details(ConversationID)
    /// Everyone the account knows by address; pushed on the phone's Home.
    case people
    /// The community calendar; pushed on the phone's Home.
    case calendar
    /// The community feed and stories; pushed on the phone's Home.
    case feed
    /// The room directory; pushed on the phone's Home.
    case browseRooms

    /// The conversation the screen belongs to, if it belongs to one.
    var conversation: ConversationID? {
        switch self {
        case let .conversation(conversation), let .thread(conversation, _), let .details(conversation):
            return conversation
        case .people, .calendar, .feed, .browseRooms:
            return nil
        }
    }

    /// The split view's section for a screen the phone pushes on Home in
    /// its place.
    var section: SplitSection? {
        switch self {
        case .people: .people
        case .calendar: .events
        case .feed: .feed
        case .browseRooms: .browseRooms
        case .conversation, .thread, .details: nil
        }
    }
}

/// Sheets presented over the shell.
enum SheetRoute: Identifiable, Hashable {
    case newMessage
    case newChannel
    case newChannelInSpace(Space)
    case leftRooms
    case profile
    case settings
    case search(ConversationID)
    case peer(PeerProfileRoute)

    var id: Self { self }
}

/// Whose profile a peer sheet shows: the name a message row shows for
/// them, their real bare JID when known, and the
/// room occupant (room and nick) the row came from, for room presence.
struct PeerProfileRoute: Hashable {
    let name: String
    let jid: BareJID?
    let room: BareJID?
    let nick: String?
    /// Their affiliation in a room they are not in now, from its member
    /// list; while they are in it, their occupant says.
    var affiliation: RoomAffiliation?
}

/// The phone tab bar: Home, DMs, Inbox and You. People, the calendar
/// and the feed open from rows on Home.
enum PhoneTab: Hashable {
    case home
    case directMessages
    case inbox
    case you
}

/// What the split shell (iPad, Mac) shows while no conversation is selected.
enum SplitSection: Hashable {
    /// Nothing picked yet.
    case home
    /// The unread overview.
    case inbox
    /// The community feed and stories.
    case feed
    /// The community calendar.
    case events
    /// Everyone the account knows by address.
    case people
    /// The room directory.
    case browseRooms
    /// The admin console, for community owners.
    case admin
}

/// A screen pushed inside the phone's You tab.
enum YouRoute: Hashable {
    case admin
}

/// Panels shown beside the conversation on iPad and Mac.
enum InspectorRoute: Hashable {
    case details
    case thread(rootID: String)
    case pins
}

/// A request to scroll a conversation to one message.
struct FocusRequest: Equatable {
    let conversation: ConversationID
    let messageID: String
}

/// Navigation for both shells. The split shell (iPad, Mac) reads
/// `selection` and `inspector`; the phone shell reads the tab and its paths.
/// `open(_:)` drives both so notifications and deep links work everywhere.
@MainActor
@Observable
final class NavigationModel {
    var selection: ConversationID? {
        didSet {
            // A thread belongs to the conversation it was opened in; details
            // and pins follow the new selection.
            if selection != oldValue, case .thread = inspector {
                inspector = nil
            }
        }
    }

    var splitSection: SplitSection = .home
    var inspector: InspectorRoute?
    var sheet: SheetRoute?

    var tab: PhoneTab = .home
    var homePath: [Route] = [] {
        didSet {
            // People, the calendar and the feed are pushed here on the
            // phone and are sections of the split view. Keep the two in
            // step, so an iPad that widens while showing one still shows
            // it, and one that left it on the phone does not come back to it.
            // `close(_:)` reassigns the path unchanged: that is no move.
            guard homePath != oldValue else { return }
            if let shown = homePath.last?.section {
                if splitSection != shown || selection != nil {
                    selection = nil
                    splitSection = shown
                }
            } else if let left = oldValue.last?.section, splitSection == left {
                splitSection = .home
            }
        }
    }
    var directPath: [Route] = []
    var inboxPath: [Route] = []
    /// The phone has no Admin tab: owners open the console from You. Kept
    /// in step with the split view, as People is on the Home stack.
    var youPath: [YouRoute] = [] {
        didSet {
            let showsAdmin = youPath.last == .admin
            if showsAdmin, splitSection != .admin || selection != nil {
                selection = nil
                splitSection = .admin
            } else if !showsAdmin, splitSection == .admin, selection == nil, oldValue.last == .admin {
                splitSection = homePath.last?.section ?? .home
            }
        }
    }

    /// Quick switcher (⌘K) visibility.
    var isQuickSwitcherPresented = false

    /// Consumed by the conversation screen showing `conversation`.
    var focusRequest: FocusRequest?

    func open(_ conversation: ConversationID, returningTo route: Route? = nil) {
        selection = conversation
        inspector = nil
        switch conversation.kind {
        case .room:
            tab = .home
            homePath = (route.map { [$0] } ?? []) + [.conversation(conversation)]
        case .direct:
            tab = .directMessages
            directPath = [.conversation(conversation)]
        }
    }

    /// Leaves the conversation on iPad and Mac for Home, Inbox, the Feed,
    /// Events, People or the admin console.
    func show(_ section: SplitSection) {
        selection = nil
        inspector = nil
        splitSection = section
        // So an iPad that shrinks to compact width lands on the same place,
        // at the root of that tab.
        switch section {
        case .home:
            tab = .home
            homePath = []
        case .inbox:
            tab = .inbox
            inboxPath = []
        case .feed:
            tab = .home
            homePath = [.feed]
        case .events:
            tab = .home
            homePath = [.calendar]
        case .people:
            tab = .home
            homePath = [.people]
        case .browseRooms:
            tab = .home
            homePath = [.browseRooms]
        case .admin:
            tab = .you
            youPath = [.admin]
        }
    }

    /// Moves away from a conversation that no longer exists, after it was
    /// deleted or left: iPad and Mac select nothing and close the
    /// inspector; each phone stack returns to the screen it was opened from.
    /// Moves off rooms the account has left or that were deleted: a
    /// composer would send to a room this device is no longer in, and a
    /// deleted room would be made again by the join that sending needs.
    func closeRooms(_ rooms: some Sequence<BareJID>) {
        rooms.map(ConversationID.room).forEach(close)
    }

    func close(_ conversation: ConversationID) {
        if selection == conversation {
            selection = nil
            inspector = nil
        }
        if case let .search(searched) = sheet, searched == conversation {
            sheet = nil
        }
        homePath = Self.path(homePath, without: conversation)
        directPath = Self.path(directPath, without: conversation)
        inboxPath = Self.path(inboxPath, without: conversation)
    }

    /// `path` up to the first screen that belongs to `conversation`.
    private static func path(_ path: [Route], without conversation: ConversationID) -> [Route] {
        guard let index = path.firstIndex(where: { $0.conversation == conversation }) else { return path }
        return Array(path[..<index])
    }

    /// Scrolls the conversation already on screen to `messageID`. Used from
    /// search and pins, which are only reachable from that conversation, so
    /// tabs and stacks stay put; on iPad/Mac the selection follows.
    func focus(_ messageID: String, in conversation: ConversationID) {
        selection = conversation
        focusRequest = FocusRequest(conversation: conversation, messageID: messageID)
    }

    func openThread(_ rootID: String, in conversation: ConversationID, usesInspector: Bool) {
        if usesInspector {
            inspector = .thread(rootID: rootID)
            return
        }
        updateTabPath { $0.append(.thread(conversation, rootID: rootID)) }
    }

    /// Goes from the thread at the end of `breadcrumb` (top-level thread
    /// first) up to `threadID`, one of the threads it hangs under. The
    /// inspector shows that thread in its place. On the phone the stack
    /// goes back to it when it is on the stack; else the sub-threads on top
    /// of the stack make way for it, so Back never leads from a thread
    /// down into one of its sub-threads.
    func openEnclosingThread(_ threadID: String, along breadcrumb: [String], in conversation: ConversationID, usesInspector: Bool) {
        if usesInspector {
            inspector = .thread(rootID: threadID)
            return
        }
        updateTabPath { path in
            guard let current = breadcrumb.last, path.last == .thread(conversation, rootID: current) else { return }
            let target = Route.thread(conversation, rootID: threadID)
            if let index = path.lastIndex(of: target) {
                path.removeSubrange((index + 1)...)
                return
            }
            let below = Set(breadcrumb.drop { $0 != threadID }.dropFirst())
            while case let .thread(onTop, id)? = path.last, onTop == conversation, below.contains(id) {
                path.removeLast()
            }
            path.append(target)
        }
    }

    /// The thread right below `rootID` on the current phone stack, when
    /// `rootID` is on top: where the navigation bar's Back goes.
    func threadBelow(_ rootID: String, in conversation: ConversationID) -> String? {
        let path: [Route]
        switch tab {
        case .home: path = homePath
        case .directMessages: path = directPath
        case .inbox: path = inboxPath
        case .you: return nil
        }
        guard path.count >= 2, path.last == .thread(conversation, rootID: rootID),
              case let .thread(below, id) = path[path.count - 2], below == conversation
        else { return nil }
        return id
    }

    /// The current phone tab's conversation stack; You has none.
    private func updateTabPath(_ update: (inout [Route]) -> Void) {
        switch tab {
        case .home: update(&homePath)
        case .directMessages: update(&directPath)
        case .inbox: update(&inboxPath)
        case .you: break
        }
    }

    func showProfile(_ profile: PeerProfileRoute) {
        sheet = .peer(profile)
    }

    func showDetails(of conversation: ConversationID, usesInspector: Bool) {
        if usesInspector {
            inspector = inspector == .details ? nil : .details
            return
        }
        updateTabPath { $0.append(.details(conversation)) }
    }
}
