import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class ComposerModelTests: XCTestCase {
    private let room = BareJID(parsing: "design@rooms.waddle.test")!
    private let conversation = ConversationID.room(BareJID(parsing: "general@rooms.waddle.test")!)
    private let jo = BareJID(parsing: "jo@waddle.test")!
    private let account = AccountIdentity(jid: BareJID(parsing: "ana@waddle.test")!, nick: "Ana")

    func testAcceptAndTypeSendOnlyThePickedRoomOccurrence() throws {
        let model = ComposerModel()
        model.replaceText(ComposerTextReplacement("#design #des"))
        try pickRoom(in: model)
        model.replaceText(ComposerTextReplacement("#design " + model.text, replacing: 0..<0))
        model.replaceText(ComposerTextReplacement(model.text + "and #design"))

        let sent = try takeMessage(from: model)
        XCTAssertEqual(sent.body, "#design #design #design and #design")
        XCTAssertEqual(sent.options.references, [roomReference(begin: 16, end: 23)])
        XCTAssertNil(model.takeSubmission(thread: nil))
    }

    func testCancellingAnEditRestoresTheDraftAndItsReferences() throws {
        let model = ComposerModel()
        model.replaceText(ComposerTextReplacement("@Jo"))
        try pickMention(in: model, selection: 3..<3)
        model.replaceText(ComposerTextReplacement(model.text + "#des"))
        try pickRoom(in: model)
        let stashedText = model.text
        let body = "#design"
        var message = WireMessage(
            type: .groupchat, from: nil, to: nil,
            identity: MessageIdentity(messageID: "edit", originID: nil, stanzaID: nil, stanzaIDs: []),
            body: body
        )
        message.references = [roomReference(begin: 0, end: 7)]
        let item = TimelineItem(
            id: "edit", conversation: conversation, isMine: true, message: message, body: body
        )

        model.beginEdit(item)
        XCTAssertEqual(model.text, body)
        model.replaceText(ComposerTextReplacement("updated #design", replacing: 0..<0))
        model.cancelEdit()

        XCTAssertEqual(model.text, stashedText)
        let sent = try takeMessage(from: model)
        XCTAssertEqual(sent.body, "@Jo #design")
        XCTAssertEqual(sent.options.references, [
            Reference(kind: .mention, uri: "xmpp:\(jo)", begin: 0, end: 3),
            roomReference(begin: 4, end: 11),
        ])
    }

    func testMeRewriteRetainsThePickedRoomAtItsNewOffset() throws {
        let model = ComposerModel()
        model.replaceText(ComposerTextReplacement("/ME   visits #des"))
        try pickRoom(in: model)
        guard case let .run(.send(body)) = SlashSubmitDecision.decide(text: model.text, extensions: [], inRoom: true) else {
            return XCTFail("Expected an action message")
        }
        model.replaceText(ComposerTextReplacement(body))
        let sent = try takeMessage(from: model)
        XCTAssertEqual(sent.body, "/me visits #design")
        XCTAssertEqual(sent.options.references, [roomReference(begin: 11, end: 18)])
    }

    private func pickRoom(in model: ComposerModel) throws {
        let suggestions = presentation(for: model)
        let candidate = try XCTUnwrap(suggestions.list.selected)
        XCTAssertEqual(candidate.kind, .room)
        model.accept(try XCTUnwrap(suggestions.pick(candidate, from: XCTUnwrap(suggestions.list.trigger), in: model.text)))
    }

    private func pickMention(in model: ComposerModel, selection: Range<Int>) throws {
        let suggestions = presentation(for: model, selection: selection)
        let candidate = try XCTUnwrap(suggestions.list.selected)
        XCTAssertEqual(candidate.kind, .mention)
        model.accept(try XCTUnwrap(suggestions.pick(candidate, from: XCTUnwrap(suggestions.list.trigger), in: model.text)))
    }

    private func presentation(for model: ComposerModel, selection: Range<Int>? = nil) -> ComposerSuggestionsModel {
        let occupant = Occupant(
            nick: "Jo", availability: .available, status: nil, affiliation: .member, role: .participant, realJID: jo, hats: []
        )
        return ComposerSuggestionsModel.present(
            draft: .init(text: model.text, selection: selection, isEditing: model.isEditing, roomLinks: model.references.roomLinks),
            place: .init(conversation: conversation, inChannel: true, account: account),
            occupants: [occupant], extensions: [], directory: nil, navigation: nil,
            rooms: [Channel(roomJID: room, name: "design")]
        )
    }

    private func takeMessage(from model: ComposerModel) throws -> (body: String, options: OutboundOptions) {
        let submission = model.takeSubmission(thread: nil)
        let draft: Draft? = if case let .send(draft) = submission { draft } else { nil }
        return try XCTUnwrap(MessageComposer.compose(try XCTUnwrap(draft)))
    }

    private func roomReference(begin: Int, end: Int) -> Reference {
        Reference(kind: .data, uri: "xmpp:\(room)?join", begin: begin, end: end)
    }
}
