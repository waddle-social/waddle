import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class MessageBodySnapshotTests: XCTestCase {
    func testRoomLinkBesideMentionAndWebLink() throws {
        let sample = SampleSession()
        let body = "@Jo, see #design and the handbook."
        let conversation = ConversationID.room(SampleSession.general)
        var message = WireMessage(
            type: .groupchat, from: SampleSession.general.with(resource: "priya"), to: nil,
            identity: MessageIdentity(messageID: "links", originID: nil, stanzaID: nil, stanzaIDs: []),
            body: body
        )
        message.references = [
            Reference(kind: .mention, uri: "xmpp:\(SampleSession.jo)", begin: 0, end: 3),
            Reference(kind: .data, uri: "xmpp:\(SampleSession.design)?join", begin: 9, end: 16),
            Reference(kind: .data, uri: "https://waddle.test/handbook", begin: 21, end: 33),
        ]
        let item = TimelineItem(id: "links", conversation: conversation, isMine: false, message: message, body: body)
        let presentation = try XCTUnwrap(MessageBodyPresentation(
            item: item, account: sample.coordinator.account,
            channels: sample.coordinator.directory.channels, actor: "Priya"
        ))
        assertHuddleSnapshot(
            of: sample.host(MessageRichBody(
                presentation: presentation, showsEditedMark: false, isSelectable: false, openRoom: { _ in /* drawn only */ }
            ))
            .padding(Theme.Spacing.m),
            width: 360
        )
    }
}
