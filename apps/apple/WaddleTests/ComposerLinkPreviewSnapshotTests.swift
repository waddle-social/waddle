import SwiftUI
import WaddleKit
import XCTest
@testable import Waddle

@MainActor
final class ComposerLinkPreviewSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let size = CGSize(width: 393, height: 245)
    #else
    private static let size = CGSize(width: 580, height: 245)
    #endif

    private let url = "https://example.org/field-guide"
    private var card: LinkPreview {
        LinkPreview(
            url: URL(string: url)!,
            title: "A field guide to city birds",
            summary: "Spot familiar birds on a walk through the neighbourhood.",
            imageURL: nil,
            imageAlt: nil,
            originalURL: url
        )
    }

    func testLoadingPreview() async throws {
        try await assertComposer(SampleSession(), result: .failed, loading: true, precision: 0.997)
    }

    func testReadyPreview() async throws {
        try await assertComposer(SampleSession(), result: .ready(payload))
    }

    func testFailedPreview() async throws {
        try await assertComposer(SampleSession(), result: .failed)
    }

    func testUnavailablePreview() async throws {
        try await assertComposer(SampleSession(), result: .unsupported)
    }

    func testRemovedPreview() async throws {
        try await assertComposer(SampleSession(), result: .failed, dismissed: true)
    }

    func testEditingWithAPreview() async throws {
        try await assertComposer(SampleSession(), result: .ready(payload), editing: true)
    }

    private var payload: LinkPreviewPayload {
        LinkPreviewPayload(token: LinkPreviewToken(rawValue: "snapshot"), expiresAt: .distantFuture, preview: card)
    }

    private func assertComposer(
        _ sample: SampleSession,
        result: LinkPreviewLookup,
        loading: Bool = false,
        dismissed: Bool = false,
        editing: Bool = false,
        precision: Float = 0.9995,
        testName: String = #function,
        line: UInt = #line
    ) async throws {
        let conversation = ConversationID.direct(SampleSession.jo)
        let model = ComposerModel()
        if loading {
            // Keep the debounce pending even under load and across themes.
            model.linkPreviews = ComposerLinkPreviewSubmission(sleep: { _ in
                let signal = AsyncStream<Void>.makeStream()
                for await _ in signal.stream { /* never yields: the lookup stays pending */ }
            })
        }
        defer { model.linkPreview.reset() }
        let text = "Guide: \(url)"
        if editing {
            var message = directMessage(
                text, from: JID(bare: SampleSession.ana.jid, resource: nil)!,
                to: JID(bare: SampleSession.jo, resource: nil)!, id: "preview-snapshot"
            )
            message.linkPreviews = [card]
            sample.coordinator.timelines.ingest(message)
            let row = try XCTUnwrap(sample.coordinator.timelines.timeline(for: conversation).items.last)
            model.beginEdit(row)
        } else {
            model.replaceText(ComposerTextReplacement(text))
        }

        sample.port.linkPreviewResult = .success(result)
        model.updateLinkPreview(in: conversation, session: sample.coordinator)
        if dismissed {
            model.linkPreview.dismiss()
        } else if !loading {
            _ = await model.linkPreview.payloadForSend(body: text, scopeJid: conversation.jid)
        }

        let composer = ConversationComposer(
            model: model, conversation: conversation, thread: nil, placeholder: "Message Jo"
        )
        .environment(MessageActionModel(conversation: conversation, composer: model, thread: nil))
        .frame(maxHeight: .infinity, alignment: .top)
        .background(Theme.Colors.bg)
        try await assertScreenSnapshot(
            of: sample.host(composer), size: Self.size, precision: precision, testName: testName, line: line
        )
    }
}
