import Foundation
import Testing
@testable import WaddleKit

private let dave = bare("dave@waddle.test")

private func occupantPresence(_ nick: String, realJID: BareJID?, codes: Set<Int> = []) -> WirePresence {
    WirePresence(
        from: room.with(resource: nick)!,
        kind: .available,
        occupant: .init(affiliation: .member, role: .participant, realJID: realJID.map { jid("\($0)/phone") }, statusCodes: codes)
    )
}

/// A nick typed without the `@` popover is still a mention: room sends
/// carry the room's mention directory, which the Rust send builder turns
/// into XEP-0372 references (wire shape asserted in waddle-xmpp-client).
@MainActor
@Suite("Typed mentions")
struct TypedMentionTests {
    private func coordinator(_ port: FakePort) -> SessionCoordinator {
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        coordinator.directory.apply(Topology(spaces: [], channels: [Channel(roomJID: room, name: "general")]))
        coordinator.handle(.presence(occupantPresence("alice", realJID: me.jid, codes: [110])))
        coordinator.handle(.presence(occupantPresence("dave", realJID: dave)))
        coordinator.handle(.presence(occupantPresence("ghost", realJID: nil)))
        return coordinator
    }

    @Test func roomSendCarriesMentionableOccupants() async {
        let port = FakePort()
        let coordinator = coordinator(port)

        await coordinator.send(Draft(text: "@dave hi"), in: roomConversation)

        #expect(port.sent.last?.options.mentionDirectory == MentionDirectory(
            targets: [MentionDirectory.Target(label: "dave", jid: dave)],
            broadcasts: true
        ))
    }

    @Test func directSendCarriesNoDirectory() async {
        let port = FakePort()
        let coordinator = coordinator(port)

        await coordinator.send(Draft(text: "@dave hi"), in: bobConversation)

        #expect(port.sent.last?.options.mentionDirectory == nil)
    }
}
