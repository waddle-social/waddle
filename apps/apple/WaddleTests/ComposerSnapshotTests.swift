import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

/// Each composer popover and its placement by the editor, in Night and Daylight.
@MainActor
final class ComposerSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let width = CGSize.phone.width
    #else
    private static let width: CGFloat = 420
    #endif

    func testFormattingBar() async throws {
        try await assertScreenSnapshot(
            of: ComposerFormattingBar(onFormat: { _ in /* drawn only */ }, onLink: { /* drawn only */ })
                .frame(height: ComposerMetrics.button)
                .frame(maxHeight: .infinity)
                .background(Theme.Colors.bg),
            size: CGSize(width: Self.width, height: 160)
        )
    }

    func testMentionSuggestions() {
        let sample = SampleSession()
        assertHuddleSnapshot(
            precision: 0.97,
            of: sample.host(ComposerSuggestionList(items: [
                .mention(.init(name: "here", target: .here)),
                .mention(.init(name: "Jo", target: .user(SampleSession.jo))),
                .mention(.init(name: "Priya", target: .user(SampleSession.priya))),
            ], selectedIndex: 1, onPick: { _ in /* drawn only */ })),
            width: Self.width
        )
    }

    func testSlashSuggestions() {
        assertHuddleSnapshot(
            precision: 0.97,
            of: ComposerSuggestionList(items: [
                .slash(.builtin(.me)),
                .slash(.builtin(.shrug)),
                .slash(.builtin(.giphy)),
            ], selectedIndex: 1, onPick: { _ in /* drawn only */ }),
            width: Self.width
        )
    }

    func testEmojiSuggestions() {
        assertHuddleSnapshot(
            precision: 0.97,
            of: ComposerSuggestionList(
                items: [
                    .emoji(.init(name: "joy", glyph: "😀")),
                    .emoji(.init(name: "face_with_tears_of_joy", glyph: "😂")),
                    .emoji(.init(name: "cat_with_tears_of_joy", glyph: "😹")),
                ],
                selectedIndex: 1, onPick: { _ in /* drawn only */ }
            ),
            width: Self.width
        )
    }

    func testRoomSuggestions() {
        let rooms = [
            Channel(roomJID: SampleSession.design, name: "design"),
            Channel(roomJID: BareJID(parsing: "design@rooms.other.test")!, name: "design"),
            Channel(roomJID: SampleSession.designCritique, name: "design-critique"),
        ]
        assertHuddleSnapshot(
            precision: 0.97,
            of: ComposerSuggestionList(
                items: RoomLinks.search(RoomLinks.candidates(from: rooms), for: "").map(ComposerSuggestion.room),
                selectedIndex: 1,
                onPick: { _ in /* drawn only */ }
            ),
            width: Self.width
        )
    }

    func testSlashSuggestionsAboveComposer() async throws {
        let sample = SampleSession()
        let messageOrder = sample.app.preferences.messageOrder
        sample.app.preferences.messageOrder = .chat
        defer { sample.app.preferences.messageOrder = messageOrder }
        let model = ComposerModel()
        model.replaceText(ComposerTextReplacement("/sh"))
        let conversation = ConversationID.room(SampleSession.general)
        let composer = ConversationComposer(
            model: model, conversation: conversation, thread: nil, placeholder: "Message #general"
        )
        .environment(MessageActionModel(conversation: conversation, composer: model, thread: nil))
        .frame(maxHeight: .infinity, alignment: .bottom)
        .background(Theme.Colors.bg)
        try await assertScreenSnapshot(
            of: sample.host(composer), size: CGSize(width: Self.width, height: 260)
        )
    }
}
