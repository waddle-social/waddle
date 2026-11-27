import Foundation
import Testing
@testable import WaddleKit
@testable import WaddleKitTestSupport

@MainActor
struct ComposerLinkPreviewSubmissionTests {
    private let first = "https://example.org/guide"

    @Test(arguments: [false, true])
    func acceptsTheFrozenDraftAndConsumesOnlyMatchingText(changesWhileWaiting: Bool) async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock(reportsDebounce: false)
        var sleeps = clock.sleeps.makeAsyncIterator()
        let previews = ComposerLinkPreviewSubmission(sleep: clock.sleep)
        let frozen = Draft(text: first)
        var editor = frozen
        let nextText = changesWhileWaiting ? "The next message" : ""
        port.sendOutcome = { message in
            #expect(!previews.isPreparing)
            #expect(editor.text == nextText)
            return .sent(stanzaID: message.clientID)
        }
        let send = Task {
            await previews.submit(
                frozen, editing: nil, in: bobConversation, session: session,
                currentDraft: { editor },
                consumeNewDraft: { if $0 { editor = Draft(text: "") } },
                perform: { draft, payload, onQueued in
                    #expect(previews.isPreparing)
                    #expect(draft == frozen)
                    await session.send(draft, in: bobConversation, linkPreview: payload, onQueued: onQueued)
                }
            )
        }

        let grace = await sleeps.next()
        #expect(grace?.duration == ComposerLinkPreview.sendLookupGrace)
        if changesWhileWaiting { editor.text = nextText }

        await previews.submit(
            editor, editing: nil, in: bobConversation, session: session,
            currentDraft: { editor }, consumeNewDraft: { _ in Issue.record("Second submit consumed the draft") },
            perform: { _, _, _ in Issue.record("Second submit reached dispatch") }
        )

        grace?.wake()
        await send.value
        #expect(port.sent.map(\.body) == [frozen.text])
        #expect(editor.text == nextText)
        #expect(previews.preview(isEditing: false).state == .idle)
        #expect(!previews.isPreparing)
        port.linkPreviewLookups.release()
    }

    @Test func stoppedSessionRejectsWithoutConsumingTheDraft() async {
        let (session, port) = onlineSession()
        let clock = PreviewTestClock(reportsDebounce: false)
        var sleeps = clock.sleeps.makeAsyncIterator()
        let previews = ComposerLinkPreviewSubmission(sleep: clock.sleep)
        var editor = Draft(text: first)
        let send = Task {
            await previews.submit(
                editor, editing: nil, in: bobConversation, session: session,
                currentDraft: { editor },
                consumeNewDraft: { if $0 { editor = Draft(text: "") } },
                perform: { draft, payload, onQueued in
                    let id = await session.send(draft, in: bobConversation, linkPreview: payload, onQueued: onQueued)
                    #expect(id == nil)
                }
            )
        }

        let grace = await sleeps.next()
        #expect(grace?.duration == ComposerLinkPreview.sendLookupGrace)
        await session.stop()
        grace?.wake()
        await send.value
        #expect(editor.text == first)
        #expect(port.sent.isEmpty)
        #expect(session.outboundQueue.isEmpty)
        #expect(!previews.isPreparing)
        previews.preview(isEditing: false).reset()
        port.linkPreviewLookups.release()
    }

    @Test(arguments: [false, true])
    func editChangedOrReplacedDuringGraceDoesNotDispatch(replacePreview: Bool) async throws {
        let (session, port) = onlineSession()
        session.route(directMessage(
            "Original", from: JID(bare: me.jid, resource: nil)!,
            to: JID(bare: bob, resource: nil)!, id: "edit-original"
        ))
        let original = try #require(session.timelines.timeline(for: bobConversation).items.last)
        let clock = PreviewTestClock(reportsDebounce: false)
        var sleeps = clock.sleeps.makeAsyncIterator()
        let previews = ComposerLinkPreviewSubmission(sleep: clock.sleep)
        var editor = Draft(text: first)
        let edit = Task {
            await previews.submit(
                editor, editing: original, in: bobConversation, session: session,
                currentDraft: { editor }, consumeNewDraft: { _ in Issue.record("Edit consumed the new-message draft") },
                perform: { _, _, _ in Issue.record("Stale edit was dispatched") }
            )
        }

        let grace = await sleeps.next()
        #expect(grace?.duration == ComposerLinkPreview.sendLookupGrace)
        if replacePreview { previews.resetEdit() } else { editor.text += " (changed)" }
        grace?.wake()
        await edit.value
        #expect(!previews.isPreparing)
        previews.resetEdit()
        port.linkPreviewLookups.release()
    }

    private func onlineSession() -> (SessionCoordinator, FakePort) {
        let port = FakePort()
        port.linkPreviewLookups.isHolding = true
        let session = SessionCoordinator(account: me, port: port)
        session.status.connection = .online
        session.isSendReady = true
        return (session, port)
    }
}
