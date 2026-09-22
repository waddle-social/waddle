import SwiftUI
import WaddleKit

extension View {
    /// Context menu for a member row: message, and the moderation actions
    /// our own occupant may take on this member.
    func memberActions(_ subject: MemberSubject, room: BareJID, model: RoomMembersModel) -> some View {
        modifier(MemberActionsMenu(subject: subject, room: room, model: model))
    }
}

struct MemberActionsMenu: ViewModifier {
    @Environment(SessionCoordinator.self) private var session
    @Environment(NavigationModel.self) private var navigation
    let subject: MemberSubject
    let room: BareJID
    let model: RoomMembersModel

    func body(content: Content) -> some View {
        content
            .contextMenu {
                if let peer = messageablePeer {
                    Button {
                        navigation.open(session.directConversation(with: peer))
                    } label: {
                        Label("Message", systemImage: "bubble.left")
                    }
                }
                if !actions.isEmpty {
                    Divider()
                    ForEach(actions, id: \.self) { action in
                        Button(role: role(for: action)) {
                            model.request(action, on: subject, using: session)
                        } label: {
                            Label(action.title, systemImage: action.symbol)
                        }
                    }
                }
            }
    }

    private var actions: [MemberAction] {
        MemberPermissions.actions(
            on: subject,
            by: session.selfOccupant(in: room),
            account: session.account
        )
    }

    /// A real JID that is not our own account.
    private var messageablePeer: BareJID? {
        guard let jid = subject.jid, jid != session.account.jid else { return nil }
        return jid
    }

    private func role(for action: MemberAction) -> ButtonRole? {
        action.isDestructive ? .destructive : nil
    }
}
