import Foundation
import Testing
@testable import WaddleKit

/// XEP-0308 corrections re-send the whole message, so the XEP-0372
/// mentions (and XEP-0394 markup) of the original must survive an edit.
@MainActor
@Suite("Editing keeps mentions")
struct EditMentionTests {
    private let carol = bare("carol@waddle.test")

    /// Sends `draft` to bob, after `earlier` arrived, and returns our row.
    private func sent(_ draft: Draft, after earlier: [WireMessage] = []) async -> (SessionCoordinator, FakePort, TimelineItem) {
        let port = FakePort()
        let coordinator = SessionCoordinator(account: me, port: port)
        coordinator.status.connection = .online
        coordinator.isSendReady = true
        earlier.forEach(coordinator.route)
        await coordinator.send(draft, in: bobConversation)
        return (coordinator, port, ownRow(coordinator))
    }

    private func ownRow(_ coordinator: SessionCoordinator) -> TimelineItem {
        coordinator.timelines.timeline(for: bobConversation).items.last { $0.isMine }!
    }

    /// What the composer sends after the user turns the editable text into
    /// `newText`: the seeded mentions re-located like new-message mentions.
    private func editDraft(_ editable: EditableMessage, to newText: String) -> Draft {
        Draft(text: newText, mentions: MentionTokens.locate(editable.recordedMentions, in: newText))
    }

    private func mention(_ target: MentionTarget, _ begin: Int, _ end: Int) -> Reference {
        Reference(kind: .mention, uri: target.uri, begin: begin, end: end)
    }

    @Test func unchangedTextKeepsMention() async {
        let (coordinator, port, item) = await sent(Draft(text: "hi @carol", mentions: [MentionDraft(target: .user(carol), range: 3..<9)]))
        let editable = EditableMessage(item: item)
        #expect(editable.text == "hi @carol")
        #expect(editable.mentions == [MentionDraft(target: .user(carol), range: 3..<9)])
        #expect(editable.recordedMentions == [RecordedMention(token: "@carol", target: .user(carol))])

        #expect(await coordinator.edit(item, draft: editDraft(editable, to: editable.text)))
        #expect(port.corrections.last?.body == "hi @carol")
        #expect(port.corrections.last?.options.references == [mention(.user(carol), 3, 9)])
    }

    @Test func textInsertedBeforeShiftsMention() async {
        let (coordinator, port, item) = await sent(Draft(text: "hi @carol", mentions: [MentionDraft(target: .user(carol), range: 3..<9)]))
        let editable = EditableMessage(item: item)
        #expect(await coordinator.edit(item, draft: editDraft(editable, to: "oh hi there @carol")))
        #expect(port.corrections.last?.options.references == [mention(.user(carol), 12, 18)])
        // 1:1 applies the correction locally, references included.
        #expect(ownRow(coordinator).message.references == [mention(.user(carol), 12, 18)])
    }

    @Test func deletedMentionTextDropsMention() async {
        let (coordinator, port, item) = await sent(Draft(text: "hi @carol", mentions: [MentionDraft(target: .user(carol), range: 3..<9)]))
        let editable = EditableMessage(item: item)
        #expect(await coordinator.edit(item, draft: editDraft(editable, to: "hi everybody")))
        #expect(port.corrections.last?.body == "hi everybody")
        #expect(port.corrections.last?.options.references == [])
    }

    @Test func mentionAfterBoldSpanLandsPastMarkers() async {
        // Wire: "hey @carol", bold 0..<3, mention 4..<10.
        let (coordinator, port, item) = await sent(Draft(text: "**hey** @carol", mentions: [MentionDraft(target: .user(carol), range: 8..<14)]))
        #expect(item.message.references == [mention(.user(carol), 4, 10)])
        let editable = EditableMessage(item: item)
        #expect(editable.text == "**hey** @carol")
        #expect(editable.mentions == [MentionDraft(target: .user(carol), range: 8..<14)])

        #expect(await coordinator.edit(item, draft: editDraft(editable, to: "**hey** there @carol")))
        let options = port.corrections.last?.options
        #expect(port.corrections.last?.body == "hey there @carol")
        #expect(options?.markupSpans == [MarkupSpan(kind: .bold, start: 0, end: 3)])
        #expect(options?.references == [mention(.user(carol), 10, 16)])
    }

    @Test func mentionWrappedInBoldSurvives() async {
        let (coordinator, port, item) = await sent(Draft(text: "**@carol** hi", mentions: [MentionDraft(target: .user(carol), range: 2..<8)]))
        let editable = EditableMessage(item: item)
        #expect(editable.text == "**@carol** hi")
        #expect(editable.mentions == [MentionDraft(target: .user(carol), range: 2..<8)])
        #expect(await coordinator.edit(item, draft: editDraft(editable, to: "**@carol** hello")))
        #expect(port.corrections.last?.options.references == [mention(.user(carol), 0, 6)])
        #expect(port.corrections.last?.options.markupSpans == [MarkupSpan(kind: .bold, start: 0, end: 6)])
    }

    @Test func broadcastMentionsSurvive() async {
        let draft = Draft(
            text: "@everyone and @here look",
            mentions: [MentionDraft(target: .everyone, range: 0..<9), MentionDraft(target: .here, range: 14..<19)]
        )
        let (coordinator, port, item) = await sent(draft)
        let editable = EditableMessage(item: item)
        #expect(editable.mentions == draft.mentions)
        #expect(await coordinator.edit(item, draft: editDraft(editable, to: "@everyone and @here look now")))
        #expect(port.corrections.last?.options.references == [mention(.everyone, 0, 9), mention(.here, 14, 19)])
    }

    @Test func replyFallbackOffsetsAreRebased() async {
        let parent = directMessage("q", from: jid("bob@waddle.test/laptop"), to: jid("alice@waddle.test/phone"), id: "p1")
        let reply = ReplyContext(targetID: "p1", author: jid("bob@waddle.test"), parentBody: "q", parentAuthorName: "bob")
        // The fallback "> q\n\n" is 5 scalars ahead of the typed body.
        let (coordinator, port, item) = await sent(
            Draft(text: "hi @carol", mentions: [MentionDraft(target: .user(carol), range: 3..<9)], reply: reply),
            after: [parent]
        )
        #expect(item.message.references == [mention(.user(carol), 8, 14)])
        let editable = EditableMessage(item: item)
        #expect(editable.mentions == [MentionDraft(target: .user(carol), range: 3..<9)])
        #expect(await coordinator.edit(item, draft: editDraft(editable, to: "yo @carol")))
        #expect(port.corrections.last?.body == "> q\n\nyo @carol")
        #expect(port.corrections.last?.options.references == [mention(.user(carol), 8, 14)])
    }

    @Test func correctedRowKeepsMentionOnSecondEdit() async {
        let (coordinator, port, item) = await sent(Draft(text: "**hi** @carol", mentions: [MentionDraft(target: .user(carol), range: 7..<13)]))
        #expect(await coordinator.edit(item, draft: editDraft(EditableMessage(item: item), to: "**hi** there @carol")))
        let corrected = ownRow(coordinator)
        #expect(corrected.isEdited)
        let editable = EditableMessage(item: corrected)
        #expect(editable.text == "**hi** there @carol")
        #expect(editable.mentions == [MentionDraft(target: .user(carol), range: 13..<19)])
        #expect(await coordinator.edit(corrected, draft: editDraft(editable, to: "**hi** again @carol")))
        #expect(port.corrections.last?.options.references == [mention(.user(carol), 9, 15)])
    }

    @Test func referencesThatDoNotMapCleanlyStayPlainText() {
        var message = directMessage("hey carol and room", from: jid("alice@waddle.test/phone"), to: jid("bob@waddle.test"), id: "m1")
        message.references = [
            // Not an `@` token.
            mention(.user(carol), 4, 9),
            // An occupant JID never widens to the room.
            Reference(kind: .mention, uri: "xmpp:general@muc.waddle.test/bob", begin: 14, end: 18),
            // Out of range.
            mention(.everyone, 30, 40),
        ]
        message.markupSpans = [MarkupSpan(kind: .bold, start: 5, end: 7)]
        let item = TimelineItem(id: "m1", conversation: bobConversation, isMine: true, message: message, body: "hey carol and room")
        let editable = EditableMessage(item: item)
        #expect(editable.text == "hey c**ar**ol and room")
        #expect(editable.mentions.isEmpty)
    }

    @Test func markerInsideMentionDropsIt() {
        var message = directMessage("hi @carol", from: jid("alice@waddle.test/phone"), to: jid("bob@waddle.test"), id: "m1")
        message.references = [mention(.user(carol), 3, 9)]
        message.markupSpans = [MarkupSpan(kind: .italic, start: 5, end: 9)]
        let item = TimelineItem(id: "m1", conversation: bobConversation, isMine: true, message: message, body: "hi @carol")
        let editable = EditableMessage(item: item)
        #expect(editable.text == "hi @c*arol*")
        #expect(editable.mentions.isEmpty)
    }

    @Test func mentionTargetReversesURI() {
        #expect(MentionTarget(reference: mention(.user(carol), 0, 1)) == .user(carol))
        #expect(MentionTarget(reference: mention(.everyone, 0, 1)) == .everyone)
        #expect(MentionTarget(reference: mention(.here, 0, 1)) == .here)
        #expect(MentionTarget(reference: Reference(kind: .data, uri: "xmpp:carol@waddle.test", begin: 0, end: 1)) == nil)
        #expect(MentionTarget(reference: Reference(kind: .mention, uri: "https://waddle.test", begin: 0, end: 1)) == nil)
    }
}
