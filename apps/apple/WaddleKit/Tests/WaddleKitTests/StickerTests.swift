import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

private let wave = Sticker(
    description: "👋",
    mediaType: "image/png",
    size: 2048,
    width: 512,
    height: 384,
    digests: FileDigests([.sha256: Data(repeating: 7, count: 32)]),
    sources: [URL(string: "https://upload.waddle.test/stickers/wave.png")!]
)

private func pack(_ id: String, name: String? = "Penguins", stickers: [Sticker] = [wave]) -> StickerPack {
    StickerPack(id: id, name: name, stickers: stickers)
}

@MainActor
@Suite("Stickers")
struct StickerTests {
    private func online() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        return (coordinator, port)
    }

    // MARK: Sending

    /// XEP-0449: the message says which pack the sticker is from, carries
    /// the picture as an inline file with its description and digest, and
    /// has the description as its text for clients without stickers.
    @Test func aStickerGoesOutAsItsPictureAndItsPack() async throws {
        let (coordinator, port) = online()

        await coordinator.send(wave, from: pack("pack-1"), in: bobConversation)

        let sent = try #require(port.sent.first)
        #expect(sent.body == "👋")
        #expect(sent.options.sticker == StickerReference(packID: "pack-1"))
        #expect(sent.options.sharedFiles == [
            SharedFile(
                url: URL(string: "https://upload.waddle.test/stickers/wave.png")!,
                mediaType: "image/png",
                size: 2048,
                width: 512,
                height: 384,
                description: "👋",
                disposition: .inline,
                digests: FileDigests([.sha256: Data(repeating: 7, count: 32)])
            ),
        ])
        #expect(sent.options.markupSpans.isEmpty)
    }

    @Test func aSentStickerShowsAtOnceAsASticker() async throws {
        let (coordinator, _) = online()

        await coordinator.send(wave, from: pack("pack-1"), in: bobConversation)

        let row = try #require(coordinator.timelines.timeline(for: bobConversation).items.last)
        #expect(row.message.isSticker)
        #expect(row.message.sharedFiles.first?.description == "👋")
    }

    /// A sticker queued while offline is saved with the outbox, so it must
    /// still be a sticker when it is read back.
    @Test func aQueuedStickerIsStillAStickerAfterBeingSaved() throws {
        let composed = try #require(MessageComposer.compose(wave, from: pack("pack-1")))
        let message = OutboundMessage(clientID: "c1", conversation: bobConversation, body: composed.body, options: composed.options)

        let restored = try JSONDecoder().decode(OutboundMessage.self, from: JSONEncoder().encode(message))

        #expect(restored == message)
        #expect(restored.options.sticker == StickerReference(packID: "pack-1"))
    }

    @Test func aReplyWithAStickerQuotesTheParentBeforeTheDescription() throws {
        let reply = ReplyContext(targetID: "m1", author: jid("bob@waddle.test"), parentBody: "Coming?", parentAuthorName: "Bob")

        let composed = try #require(MessageComposer.compose(wave, from: pack("pack-1"), reply: reply, thread: "t1"))

        #expect(composed.body == "> Coming?\n\n👋")
        #expect(composed.options.reply == OutboundOptions.Reply(targetID: "m1", author: jid("bob@waddle.test"), fallback: 0..<11))
        #expect(composed.options.thread == "t1")
    }

    /// A correction re-sends the message as text, which would turn the
    /// sticker into its description.
    @Test func aSentStickerCannotBeEdited() async throws {
        let (coordinator, port) = online()
        await coordinator.send(wave, from: pack("pack-1"), in: bobConversation)
        let row = try #require(coordinator.timelines.timeline(for: bobConversation).items.last)

        let edited = await coordinator.edit(row, draft: Draft(text: "hello"))

        #expect(!edited)
        #expect(port.corrections.isEmpty)
    }

    // MARK: Packs

    @Test func loadingShowsThePacksOfTheAccount() async {
        let (coordinator, port) = online()
        port.stickers.packs = .success([pack("a"), pack("b", name: "Seals")])

        await coordinator.loadStickerPacks()

        #expect(coordinator.stickers.packs.map(\.id) == ["a", "b"])
        #expect(coordinator.stickers.phase == .loaded)
    }

    @Test func aFailedLoadKeepsThePacksAlreadyShown() async {
        let (coordinator, port) = online()
        port.stickers.packs = .success([pack("a")])
        await coordinator.loadStickerPacks()

        port.stickers.packs = .failure(.timeout)
        await coordinator.loadStickerPacks()

        #expect(coordinator.stickers.packs.map(\.id) == ["a"])
        #expect(coordinator.stickers.phase == .failed)
    }

    @Test func loadingOfflineAsksNothingAndFails() async {
        let (coordinator, port) = online()
        coordinator.status.connection = .offline(retryAt: nil)

        await coordinator.loadStickerPacks()

        #expect(port.stickers.packRequests == 0)
        #expect(coordinator.stickers.phase == .failed)
    }

    /// The core derives a pack's id from its content, so the new pack is
    /// read back under the id the publish returned.
    @Test func aNewPackIsPublishedAndShownAsTheServerHoldsIt() async throws {
        let (coordinator, port) = online()
        port.stickers.packs = .success([pack("a")])
        await coordinator.loadStickerPacks()
        port.stickers.publishedID = "fresh"
        let draft = StickerPackDraft(name: "Seals", summary: "Flippers", stickers: [wave])

        let created = try await coordinator.createStickerPack(draft)

        #expect(port.stickers.published == [draft])
        #expect(created == StickerPack(id: "fresh", name: "Seals", summary: "Flippers", stickers: [wave]))
        #expect(coordinator.stickers.packs.map(\.id) == ["a", "fresh"])
    }

    @Test func aPackTheServerRefusesIsNotShown() async {
        let (coordinator, port) = online()
        port.stickers.publishResult = .failure(.rejected)

        await #expect(throws: PortError.rejected) {
            try await coordinator.createStickerPack(StickerPackDraft(name: "Seals", stickers: [wave]))
        }

        #expect(coordinator.stickers.packs.isEmpty)
    }

    @Test func aPackWithoutANameOrStickersIsNotPublished() async {
        let (coordinator, port) = online()

        await #expect(throws: PortError.invalidRequest) {
            try await coordinator.createStickerPack(StickerPackDraft(name: "  ", stickers: [wave]))
        }
        await #expect(throws: PortError.invalidRequest) {
            try await coordinator.createStickerPack(StickerPackDraft(name: "Seals", stickers: []))
        }

        #expect(port.stickers.published.isEmpty)
    }

    @Test func removingAPackTakesItAway() async throws {
        let (coordinator, port) = online()
        port.stickers.packs = .success([pack("a"), pack("b")])
        await coordinator.loadStickerPacks()

        try await coordinator.removeStickerPack("a")

        #expect(port.stickers.retracted == ["a"])
        #expect(coordinator.stickers.packs.map(\.id) == ["b"])
    }

    @Test func aPackThatCouldNotBeRemovedStays() async {
        let (coordinator, port) = online()
        port.stickers.packs = .success([pack("a")])
        await coordinator.loadStickerPacks()
        port.stickers.retractResult = .failure(.timeout)

        await #expect(throws: PortError.timeout) { try await coordinator.removeStickerPack("a") }

        #expect(coordinator.stickers.packs.map(\.id) == ["a"])
    }
}
