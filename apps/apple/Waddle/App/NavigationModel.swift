import Foundation
import Observation
import WaddleKit

/// A screen pushed inside a navigation stack.
enum Route: Hashable {
    case conversation(ConversationID)
    case thread(ConversationID, rootID: String)
    case details(ConversationID)
    /// Everyone the account knows by address; pushed on the Home stack.
    case people

    /// The conversation the screen belongs to, if it belongs to one.
    var conversation: ConversationID? {
        switch self {
        case let .conversation(conversation), let .thread(conversation, _), let .details(conversation):
            return conversation
        case .people:
            return nil
        }
    }
}

/// Sheets presented over the shell.
enum SheetRoute: Identifiable, Hashable {
    case newMessage
    case newChannel
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
}

/// The phone tab bar.
enum PhoneTab: Hashable {
    case home
    case directMessages
    case inbox
    case feed
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
    /// The admin console, for community owners.
    case admin
}

/// A screen pushed inside the phone's Feed tab.
enum FeedRoute: Hashable {
    case events
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
            // People is pushed here on the phone and is a section of the
            // split view. Keep the two in step, so an iPad that widens
            // while showing People still shows it, and one that left
            // People on the phone does not come back to it.
            let showsPeople = homePath.last == .people
            if showsPeople, splitSection != .people || selection != nil {
                selection = nil
                splitSection = .people
            } else if !showsPeople, splitSection == .people, selection == nil, oldValue.last == .people {
                splitSection = .home
            }
        }
    }
    var directPath: [Route] = []
    var inboxPath: [Route] = []
    /// The phone has no Events tab: Events opens from the Feed.
    var feedPath: [FeedRoute] = []
    /// The phone has no Admin tab: owners open the console from You. Kept
    /// in step with the split view, as People is on the Home stack.
    var youPath: [YouRoute] = [] {
        didSet {
            let showsAdmin = youPath.last == .admin
            if showsAdmin, splitSection != .admin || selection != nil {
                selection = nil
                splitSection = .admin
            } else if !showsAdmin, splitSection == .admin, selection == nil, oldValue.last == .admin {
                splitSection = homePath.last == .people ? .people : .home
            }
        }
    }

    /// Quick switcher (⌘K) visibility.
    var isQuickSwitcherPresented = false

    /// Consumed by the conversation screen showing `conversation`.
    var focusRequest: FocusRequest?

    func open(_ conversation: ConversationID) {
        selection = conversation
        inspector = nil
        switch conversation.kind {
        case .room:
            tab = .home
            homePath = [.conversation(conversation)]
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
            tab = .feed
            feedPath = []
        case .events:
            tab = .feed
            feedPath = [.events]
        case .people:
            tab = .home
            homePath = [.people]
        case .admin:
            tab = .you
            youPath = [.admin]
        }
    }

    /// Moves away from a conversation that no longer exists, after it was
    /// deleted or left: iPad and Mac select nothing and close the
    /// inspector; each phone stack returns to the screen it was opened from.
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
        switch tab {
        case .home: homePath.append(.thread(conversation, rootID: rootID))
        case .directMessages: directPath.append(.thread(conversation, rootID: rootID))
        case .inbox: inboxPath.append(.thread(conversation, rootID: rootID))
        case .feed, .you: break
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
        switch tab {
        case .home: homePath.append(.details(conversation))
        case .directMessages: directPath.append(.details(conversation))
        case .inbox: inboxPath.append(.details(conversation))
        case .feed, .you: break
        }
    }
}
