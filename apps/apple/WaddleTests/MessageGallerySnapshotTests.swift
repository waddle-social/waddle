import SwiftUI
import WaddleKit
import XCTest

@testable import Waddle

@MainActor
final class MessageGallerySnapshotTests: XCTestCase {
    func testMiddleImage() async throws {
        try await assertGallery(selected: 1)
    }

    func testFirstImage() async throws {
        try await assertGallery(selected: 0)
    }

    func testLastImage() async throws {
        try await assertGallery(selected: 2)
    }

    func testSingleImage() async throws {
        try await assertGallery(selected: 0, names: ["Lakeside.png"])
    }

    func testImageThatDidNotLoad() async throws {
        let missing = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
            .appendingPathComponent("Meadow.gif")
        let gallery = try makeGallery(urls: [missing], selected: 0)
        try await assertStage(gallery, size: .desktop, picture: nil)
    }

    #if os(iOS)
    func testMiddleImageOnPhone() async throws {
        try await assertGallery(selected: 1, size: .phone)
    }
    #endif

    private func assertGallery(
        selected: Int,
        names: [String] = ["Lakeside.png", "Meadow.png", "Mountain pass.png"],
        size: CGSize = .desktop,
        testName: String = #function,
        line: UInt = #line
    ) async throws {
        let base = URL(string: "https://files.waddle.test/")!
        let gallery = try makeGallery(urls: names.map { base.appendingPathComponent($0) }, selected: selected)
        try await assertStage(gallery, size: size, picture: Self.picture, testName: testName, line: line)
    }

    private func assertStage(
        _ gallery: MediaGallery,
        size: CGSize,
        picture: Image?,
        testName: String = #function,
        line: UInt = #line
    ) async throws {
        let stage = MediaGalleryView(gallery: gallery, select: { _ in }, close: {}, standIn: picture)
        #if os(iOS)
        let view = stage.environment(\.horizontalSizeClass, size == .phone ? .compact : .regular)
        #else
        let view = stage
        #endif
        try await assertScreenSnapshot(of: view, size: size, testName: testName, line: line)
    }

    private func makeGallery(urls: [URL], selected: Int) throws -> MediaGallery {
        let sender = JID(parsing: "sam@waddle.test/phone")!
        let items = urls.enumerated().map { index, url in
            let id = "gallery-\(index)"
            let file = SharedFile(url: url, name: url.lastPathComponent, mediaType: "image/\(url.pathExtension)")
            let message = WireMessage(
                type: .chat, from: sender, to: nil, identity: MessageIdentity(), body: "", sharedFiles: [file]
            )
            return TimelineItem(
                id: id, presentationID: id, conversation: .direct(sender.bare), isMine: false, message: message, body: ""
            )
        }
        let row = items[selected]
        let image = try XCTUnwrap(MediaGallery.images(in: row, media: MessageMedia(item: row, decision: .allowed)).first)
        return try XCTUnwrap(MediaGallery(items: items, selected: image.id) { _ in .allowed })
    }

    private static let picture: Image = {
        let context = CGContext(
            data: nil, width: 480, height: 270, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        context.setFillColor(red: 0.61, green: 0.79, blue: 0.87, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: 480, height: 270))
        context.setFillColor(red: 1, green: 0.90, blue: 0.65, alpha: 1)
        context.fillEllipse(in: CGRect(x: 325, y: 168, width: 62, height: 62))
        context.setFillColor(red: 0.15, green: 0.31, blue: 0.37, alpha: 1)
        context.addLines(between: [
            CGPoint(x: 0, y: 82), CGPoint(x: 142, y: 192), CGPoint(x: 234, y: 99),
            CGPoint(x: 304, y: 143), CGPoint(x: 480, y: 47), CGPoint(x: 480, y: 0), CGPoint(x: 0, y: 0),
        ])
        context.fillPath()
        return Image(decorative: context.makeImage()!, scale: 1)
    }()
}
