import Foundation
import Testing
import WaddleKit
@testable import WaddleKitTestSupport

struct MessageRoomLinksTests {
    private let ops = bare("ops@rooms.waddle.test")
    private let lab = bare("lab@rooms.waddle.test")

    @Test func actionBodySharesRoomLinksBetweenRunsAndVoiceOver() {
        let item = row(
            "/me 🇳🇴 #ops",
            references: [reference(ops, 12..<16), reference(lab, 5..<9)],
            reply: "> 🙂\n\n"
        )
        let blocks = RichTextLayout.blocks(for: item, account: me)
        #expect(blocks == [.paragraph([
            RichSegment(text: "🇳🇴 ", styles: []),
            RichSegment(text: "#ops", styles: [.roomLink(ops)]),
        ])])
        let links = MessageRoomLinks(blocks: blocks, channels: channels)
        #expect(links.actions.map(\.room) == [ops])
        #expect(links.actions.map(\.name) == ["Ops"])
    }

    @Test func roomReferencesOverlappingAMentionOrRunningOutOfBoundsStayPlain() {
        let item = row("@Jo #lab #ops", references: [
            reference(ops, 0..<3),
            Reference(kind: .mention, uri: "xmpp:\(me.jid)", begin: 0, end: 3),
            reference(lab, 4..<8),
            reference(ops, 9..<99),
        ])
        let blocks = RichTextLayout.blocks(for: item, account: me)
        #expect(blocks == [.paragraph([
            RichSegment(text: "@Jo", styles: [.mention(.me)]),
            RichSegment(text: " ", styles: []),
            RichSegment(text: "#lab", styles: [.roomLink(lab)]),
            RichSegment(text: " #ops", styles: []),
        ])])
        #expect(MessageRoomLinks(blocks: blocks, channels: channels).actions.map(\.room) == [lab])
    }

    @Test func inlineAndBlockCodeHaveNoRoomActions() {
        let item = row("#ops\n#lab\n#ops", references: [
            reference(ops, 0..<4), reference(lab, 5..<9), reference(ops, 10..<14),
        ], spans: [
            MarkupSpan(kind: .code, start: 0, end: 4),
            MarkupSpan(kind: .codeBlock, start: 5, end: 9),
        ])
        let blocks = RichTextLayout.blocks(for: item, account: me)
        #expect(blocks == [
            .paragraph([RichSegment(text: "#ops", styles: [.code])]),
            .code("#lab"),
            .paragraph([RichSegment(text: "#ops", styles: [.roomLink(ops)])]),
        ])
        #expect(MessageRoomLinks(blocks: blocks, channels: channels).actions.map(\.room) == [ops])
    }

    @Test func trustedLinksOpenKnownRoomsAndOtherURLSchemesPassThrough() throws {
        let address = bare("café+build@[::1]")
        let blocks = RichTextLayout.blocks(for: row("#build", references: [reference(address, 0..<6)]), account: me)
        let channel = Channel(roomJID: address, name: "Build")
        let links = MessageRoomLinks(blocks: blocks, channels: [channel])
        let url = try #require(links.url(for: address))
        #expect(url.scheme == "waddle-room")
        #expect(links.destination(for: url) == .room(address))

        let unavailable = MessageRoomLinks(blocks: blocks, channels: [channel.with(hasLeft: true)])
        #expect(unavailable.url(for: address) == nil)
        #expect(unavailable.destination(for: url) == .discard)
        #expect(unavailable.actions.isEmpty)

        for value in ["xmpp:someone@waddle.test?message", "https://waddle.test/rooms", "mailto:someone@waddle.test"] {
            let external = try #require(URL(string: value))
            let item = row("other", spans: [MarkupSpan(kind: .link(external), start: 0, end: 5)])
            #expect(RichTextLayout.blocks(for: item, account: me) == [
                .paragraph([RichSegment(text: "other", styles: [.link(external)])]),
            ])
            #expect(links.destination(for: external) == .external)
        }
    }

    @Test func aCraftedURLCannotBorrowAnActualRoomLinkOnTheSameRow() throws {
        let genuine = RichTextLayout.blocks(for: row("#ops", references: [reference(ops, 0..<4)]), account: me)
        let url = try #require(MessageRoomLinks(blocks: genuine, channels: channels).url(for: ops))
        let item = row("#ops " + url.absoluteString, references: [reference(ops, 0..<4)], spans: [
            MarkupSpan(kind: .link(url), start: 5, end: 5 + url.absoluteString.unicodeScalars.count),
        ])
        let blocks = RichTextLayout.blocks(for: item, account: me) { text in
            guard text == item.body else { return [] }
            return [RichDetectedLink(range: 5..<text.unicodeScalars.count, url: url)]
        }
        #expect(blocks == [.paragraph([
            RichSegment(text: "#ops", styles: [.roomLink(ops)]),
            RichSegment(text: " " + url.absoluteString, styles: []),
        ])])
        let plain = RichTextLayout.blocks(for: row(url.absoluteString), account: me)
        #expect(MessageRoomLinks(blocks: plain, channels: channels).destination(for: url) == .discard)
    }

    @Test func roomsWithDuplicateDisplayNamesGetDistinctActions() {
        let item = row("#Ops #Ops #Ops", references: [
            reference(ops, 0..<4), reference(lab, 5..<9), reference(ops, 10..<14),
        ])
        let blocks = RichTextLayout.blocks(for: item, account: me)
        let links = MessageRoomLinks(blocks: blocks, channels: [
            Channel(roomJID: ops, name: "Ops"), Channel(roomJID: lab, name: "Ops"),
        ])
        #expect(links.actions.map(\.name) == ["Ops, ops@rooms.waddle.test", "Ops, lab@rooms.waddle.test"])
    }

    private var channels: [Channel] {
        [Channel(roomJID: ops, name: "Ops"), Channel(roomJID: lab, name: "Lab")]
    }

    private func reference(_ room: BareJID, _ range: Range<Int>) -> Reference {
        Reference(kind: .data, uri: RoomLinks.joinURI(for: room)!, begin: range.lowerBound, end: range.upperBound)
    }

    private func row(
        _ body: String, references: [Reference] = [], spans: [MarkupSpan] = [], reply: String = ""
    ) -> TimelineItem {
        let conversation = ConversationID.room(ops)
        var message = WireMessage(
            type: .groupchat, from: ops.with(resource: "jo"), to: nil,
            identity: MessageIdentity(messageID: "links", originID: nil, stanzaID: nil, stanzaIDs: []),
            body: reply + body
        )
        message.references = references
        message.markupSpans = spans
        if !reply.isEmpty {
            message.reply = .init(id: "parent", author: nil, fallback: 0..<reply.unicodeScalars.count)
        }
        return TimelineItem(id: "links", conversation: conversation, isMine: false, message: message, body: body)
    }
}
