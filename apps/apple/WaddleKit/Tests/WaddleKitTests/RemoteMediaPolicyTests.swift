import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct RemoteMediaPolicyTests {
    private let home = RemoteMediaPolicy(account: me.jid)

    @Test(arguments: ["dev@chat.remote.test/bob", "dev@chat.remote.test"])
    func foreignRoomTakesPriorityOverTheSender(from: String) {
        var message = roomRow(in: foreignRoom, sender: bob)
        message.message.from = jid(from)
        #expect(home.decision(for: message) == .withheld(host: Host("chat.remote.test")))
    }

    @Test func aHomeSenderInAHomeRoomLoads() {
        #expect(home.decision(for: roomRow(sender: bob)) == .allowed)
    }

    @Test func anAnonymousOccupantInAHomeRoomLoads() {
        #expect(home.decision(for: roomRow()) == .allowed)
    }

    @Test func aForeignSenderInAHomeRoomWaits() {
        #expect(home.decision(for: roomRow(sender: foreignSender)) == .withheld(host: Host("remote.test")))
    }

    @Test func aForeignDirectMessageWaits() {
        #expect(home.decision(for: directRow()) == .withheld(host: Host("remote.test")))
    }

    @Test func anUnidentifiedDirectSenderWaits() {
        var message = directRow()
        message.message.from = nil
        #expect(home.decision(for: message) == .withheld(host: nil))
    }

    @Test func discoveryTrustsOnlyProvenHomeServiceHosts() {
        let session = SessionCoordinator(account: me, port: FakePort())
        let discovered = Space(id: "home", serviceJID: bare("community.example"), name: "Home", summary: nil)
        let conventional = Space(id: "standalone", serviceJID: bare("muc.waddle.test"), name: "Rooms", summary: nil)
        session.directory.apply(Topology(spaces: [discovered, conventional], channels: [
            Channel(roomJID: foreignRoom.jid, name: "Foreign room", spaceID: discovered.id),
        ]))
        #expect(session.remoteMediaDecision(for: directRow(from: bare("bot@community.example"))) == .withheld(host: Host("community.example")))
        #expect(session.remoteMediaDecision(for: roomRow(in: foreignRoom)) == .withheld(host: Host("chat.remote.test")))
        #expect(session.remoteMediaDecision(for: roomRow(in: .room(bare("dev@community.example")))) == .withheld(host: Host("community.example")))
        #expect(session.remoteMediaDecision(for: roomRow()) == .allowed)
    }

    @Test func anExtensionCommandDoesNotMakeItsHostHome() {
        let session = SessionCoordinator(account: me, port: FakePort())
        session.extensionCommands = [ExtensionCommand(
            serviceJID: "commands.example", node: "poll", name: "Poll", scope: .channel,
            composerPrefix: "poll", inlineField: nil, composerExecute: false
        )]

        let row = roomRow(sender: bare("bot@commands.example"))
        #expect(session.remoteMediaDecision(for: row) == .withheld(host: Host("commands.example")))
    }

    @Test func aMediaURLDoesNotEstablishHomeTrust() {
        var homeSender = directRow(from: bob)
        homeSender.body = "https://uploads.example/photo.png"
        #expect(home.decision(for: homeSender) == .allowed)

        var foreignUploader = directRow(from: bare("bot@uploads.example"))
        foreignUploader.body = homeSender.body
        #expect(home.decision(for: foreignUploader) == .withheld(host: Host("uploads.example")))
        foreignUploader.body = "https://uploads.waddle.test/photo.png"
        #expect(home.decision(for: foreignUploader) == .withheld(host: Host("uploads.example")))
    }

    @Test func aDifferentUnicodeSpellingIsOutsideTheHomeDomain() {
        let account = AccountIdentity(jid: bare("alice@faß.test"), nick: "alice")
        let session = SessionCoordinator(account: account, port: FakePort())
        let conventional = Space(id: "standalone", serviceJID: bare("muc.fass.test"), name: "Rooms", summary: nil)
        session.directory.apply(Topology(spaces: [conventional], channels: []))

        let room = roomRow(in: .room(bare("dev@muc.fass.test")))
        #expect(session.remoteMediaDecision(for: room) == .withheld(host: Host("muc.fass.test")))
    }

    @Test(arguments: ["waddle.test", "muc.waddle.test", "spaces.WADDLE.TEST."])
    func roomsAreHomeOnTheAccountDomainAndItsTwoServiceNames(host: String) {
        #expect(home.decision(for: roomRow(in: .room(bare("dev@\(host)")))) == .allowed)
    }

    /// Tenants are named by domain, and one can sit under another's: a name below the account's
    /// domain is another server unless it is one of the home server's own two.
    @Test(arguments: ["acme.waddle.test", "rooms.eu.waddle.test", "muc.acme.waddle.test", "xwaddle.test", "localhost", "192.0.2.7"])
    func otherNamesBelowOrBesideTheAccountDomainAreForeignRooms(host: String) {
        #expect(home.decision(for: roomRow(in: .room(bare("dev@\(host)")))) != .allowed)
    }

    @Test func aPersonIsHomeOnlyOnTheAccountDomainItself() {
        #expect(home.decision(for: directRow(from: bare("bot@WADDLE.TEST."))) == .allowed)
        for host in ["muc.waddle.test", "spaces.waddle.test", "acme.waddle.test"] {
            #expect(home.decision(for: directRow(from: bare("bot@\(host)"))) != .allowed)
        }
    }

    @Test(arguments: ["evil-waddle.test", "waddle.test.evil.net", "muc.waddle.test.."])
    func similarDomainsAreForeign(host: String) {
        let room = roomRow(in: .room(bare("dev@\(host)")))
        #expect(home.decision(for: room) != .allowed)
        #expect(home.decision(for: directRow(from: bare("bot@\(host)"))) != .allowed)
    }

    @Test func aSharedParentDomainIsNotHome() {
        let policy = RemoteMediaPolicy(account: bare("alice@CO.UK."))
        #expect(policy.decision(for: roomRow(in: .room(bare("dev@ROOMS.CO.UK")))) != .allowed)
        #expect(policy.decision(for: roomRow(in: .room(bare("dev@MUC.CO.UK")))) == .allowed)
    }
}
