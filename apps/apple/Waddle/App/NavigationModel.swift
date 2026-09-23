import Foundation
import Observation
import WaddleKit

/// A screen pushed inside a navigation stack.
enum Route: Hashable {
    case conversation(ConversationID)
    case thread(ConversationID, rootID: String)
    case details(ConversationID)
}

/// Sheets presented over the shell.
enum SheetRoute: Identifiable, Hashable {
    case newMessage
    case newChannel
    case profile
    case settings
    case search(ConversationID)

    var id: Self { self }
}

/// The phone tab bar.
enum PhoneTab: Hashable {
    case home
    case directMessages
    case activity
    case you
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
    var selection: ConversationID?
    var inspector: InspectorRoute?
    var sheet: SheetRoute?

    var tab: PhoneTab = .home
    var homePath: [Route] = []
    var directPath: [Route] = []
    var activityPath: [Route] = []

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
        case .activity: activityPath.append(.thread(conversation, rootID: rootID))
        case .you: break
        }
    }

    func showDetails(of conversation: ConversationID, usesInspector: Bool) {
        if usesInspector {
            inspector = inspector == .details ? nil : .details
            return
        }
        switch tab {
        case .home: homePath.append(.details(conversation))
        case .directMessages: directPath.append(.details(conversation))
        case .activity: activityPath.append(.details(conversation))
        case .you: break
        }
    }
}
