import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let wave = Sticker(
    description: "👋",
    mediaType: "image/png",
    digests: FileDigests([.sha256: Data(repeating: 7, count: 32)]),
    sources: [URL(string: "https://upload.waddle.test/stickers/wave.png")!]
)

/// A sticker message from `from`, naming `pack`.
private func sticker(_ message: WireMessage, pack: StickerReference?) -> WireMessage {
    var message = message
    message.isSticker = true
    message.stickerPack = pack
    return message
}

@MainActor
@Suite("The pack behind a sticker")
struct StickerPackBehindTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        return (coordinator, port)
    }

    private func row(_ message: WireMessage, in conversation: ConversationID, of coordinator: SessionCoordinator) throws -> TimelineItem {
        _ = coordinator.timelines.ingest(message)
        return try #require(coordinator.timelines.timeline(for: conversation).items.last)
    }

    // MARK: Where the pack is

    /// XEP-0449: a marker with only a pack id means the sender's own packs.
    @Test func aStickerFromSomeoneIsFromTheirOwnPacks() throws {
        let (coordinator, _) = online()
        let message = sticker(
            directMessage("👋", from: jid("bob@waddle.test/phone"), to: jid("alice@waddle.test"), id: "m1"),
            pack: StickerReference(packID: "pack-1")
        )

        let item = try row(message, in: bobConversation, of: coordinator)

        #expect(coordinator.stickerPackLocation(of: item) == StickerPackLocation(owner: bob, packID: "pack-1"))
    }

    @Test func aMarkerThatSaysWhereThePackIsWinsOverTheSender() throws {
        let (coordinator, _) = online()
        let message = sticker(
            directMessage("👋", from: jid("bob@waddle.test/phone"), to: jid("alice@waddle.test"), id: "m1"),
            pack: StickerReference(packID: "pack-1", owner: bare("packs.waddle.test"), node: "shared/packs")
        )

        let item = try row(message, in: bobConversation, of: coordinator)

        #expect(coordinator.stickerPackLocation(of: item)
            == StickerPackLocation(owner: bare("packs.waddle.test"), node: "shared/packs", packID: "pack-1"))
    }

    /// In a room the sender is a nick; only the account the room vouches
    /// for can be asked for its packs.
    @Test func inARoomThePackIsOnTheAccountBehindTheNick() throws {
        let (coordinator, _) = online()
        var known = sticker(roomMessage("👋", from: "bob", stanzaID: "s1"), pack: StickerReference(packID: "pack-1"))
        known.authorRealJID = bob
        let unknown = sticker(roomMessage("👋", from: "carol", stanzaID: "s2"), pack: StickerReference(packID: "pack-2"))

        let knownItem = try row(known, in: roomConversation, of: coordinator)
        let unknownItem = try row(unknown, in: roomConversation, of: coordinator)

        #expect(coordinator.stickerPackLocation(of: knownItem) == StickerPackLocation(owner: bob, packID: "pack-1"))
        #expect(coordinator.stickerPackLocation(of: unknownItem) == nil)
    }

    @Test func aStickerISentIsFromMyOwnPacks() async throws {
        let (coordinator, _) = online()
        await coordinator.send(wave, from: StickerPack(id: "mine", stickers: [wave]), in: bobConversation)

        let item = try #require(coordinator.timelines.timeline(for: bobConversation).items.last)

        #expect(coordinator.stickerPackLocation(of: item) == StickerPackLocation(owner: me.jid, packID: "mine"))
    }

    @Test func aStickerThatNamesNoPackAndAnOrdinaryMessageHaveNone() throws {
        let (coordinator, _) = online()
        let bare = sticker(
            directMessage("👋", from: jid("bob@waddle.test/phone"), to: jid("alice@waddle.test"), id: "m1"),
            pack: nil
        )
        // A pack reference on a message that is not a sticker is ignored.
        var plain = directMessage("hello", from: jid("bob@waddle.test/phone"), to: jid("alice@waddle.test"), id: "m2")
        plain.stickerPack = StickerReference(packID: "pack-1")

        let bareItem = try row(bare, in: bobConversation, of: coordinator)
        let plainItem = try row(plain, in: bobConversation, of: coordinator)

        #expect(coordinator.stickerPackLocation(of: bareItem) == nil)
        #expect(coordinator.stickerPackLocation(of: plainItem) == nil)
    }

    // MARK: Opening it

    @Test func openingAPackAsksItsOwnerForIt() async throws {
        let (coordinator, port) = online()
        let location = StickerPackLocation(owner: bob, packID: "pack-1")
        let pack = StickerPack(id: "pack-1", name: "Seals", stickers: [wave])
        port.stickers.packsElsewhere[location] = .success(pack)

        let opened = try await coordinator.stickerPack(at: location)

        #expect(opened == pack)
        #expect(port.stickers.packLookups == [location])
    }

    @Test func aPackThatIsGoneOpensAsNothing() async throws {
        let (coordinator, _) = online()

        let opened = try await coordinator.stickerPack(at: StickerPackLocation(owner: bob, packID: "gone"))

        #expect(opened == nil)
    }

    @Test func openingAPackOfflineFails() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await #expect(throws: PortError.notConnected) {
            try await coordinator.stickerPack(at: StickerPackLocation(owner: bob, packID: "pack-1"))
        }
        #expect(port.stickers.packLookups.isEmpty)
    }

    // MARK: Copying it

    /// XEP-0449 "Importing a sticker pack": the pack is published again on
    /// the account's own packs, with the same stickers.
    @Test func copyingAPackPublishesItToMyOwnPacks() async throws {
        let (coordinator, port) = online()
        port.stickers.publishedID = "pack-1"
        let theirs = StickerPack(id: "pack-1", name: "Seals", summary: "Flippers", stickers: [wave])

        let mine = try await coordinator.copyStickerPack(theirs)

        #expect(port.stickers.published == [StickerPackDraft(name: "Seals", summary: "Flippers", stickers: [wave])])
        #expect(mine.id == "pack-1")
        #expect(coordinator.stickers.packs.map(\.id) == ["pack-1"])
    }

    /// The XEP asks apps not to offer copying a pack its author restricted.
    @Test func aPackItsAuthorKeepsToThemselvesIsNotCopied() async {
        let (coordinator, port) = online()
        let theirs = StickerPack(id: "pack-1", name: "Seals", isRestricted: true, stickers: [wave])

        await #expect(throws: PortError.rejected) { try await coordinator.copyStickerPack(theirs) }

        #expect(port.stickers.published.isEmpty)
        #expect(coordinator.stickers.packs.isEmpty)
    }

    @Test func aPackWithoutANameIsCopiedAsStickers() async throws {
        let (coordinator, port) = online()

        try await coordinator.copyStickerPack(StickerPack(id: "pack-1", stickers: [wave]))

        #expect(port.stickers.published.map(\.name) == ["Stickers"])
    }
}
