import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for stickers: the picker with packs, with
/// none and after a failed load, the new-pack sheet before and after
/// pictures are chosen, and the pack behind a sticker in a conversation. These screens have no design frame; they are
/// drawn from the styleguide's components. Text fields and scroll views
/// only draw in a real window, so these are screen snapshots, compared on
/// the OS the goldens were recorded on (see `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/StickerSnapshotTests` and run the tests once to record
/// new ones (that run fails), then commit them.
@MainActor
final class StickerSnapshotTests: XCTestCase {
    #if os(iOS)
    private static let sheet = CGSize.phone
    #else
    private static let sheet = CGSize(width: 440, height: 600)
    #endif

    // MARK: - Picker

    func testPickerWithTwoPacks() async throws {
        let sample = SampleSession()
        sample.port.stickers.packs = .success([
            StickerPack(
                id: "penguins",
                name: "Penguins",
                summary: "For when words are too slow.",
                stickers: ["👋", "🎉", "😴", "🐟", "❄️", "💙", "🙌"].map(Self.sticker)
            ),
            StickerPack(id: "release", name: "Release train", stickers: ["🚂"].map(Self.sticker)),
        ])
        try await assertPicker(sample)
    }

    func testPickerWithNoPacks() async throws {
        try await assertPicker(SampleSession())
    }

    func testPickerThatDidNotLoad() async throws {
        let sample = SampleSession()
        sample.port.stickers.packs = .failure(.timeout)
        try await assertPicker(sample)
    }

    private func assertPicker(_ sample: SampleSession, testName: String = #function) async throws {
        let picker = StickerPickerSheet(onPick: { _, _ in }, onCancel: {}, samplePicture: Self.picture)
        try await assertScreenSnapshot(of: sample.host(picker), size: Self.sheet, asSheet: true, testName: testName)
    }

    // MARK: - New pack

    func testNewPackWithNothingChosen() async throws {
        try await assertComposer(StickerPackComposer())
    }

    func testNewPackWithPictures() async throws {
        let payload = AttachmentPayload(
            data: Self.pictureData, filename: "sticker-1.png", mediaType: "image/png", width: 256, height: 256
        )
        var wave = StickerPackComposer.Entry(payload: payload, preview: Self.picture)
        wave.description = "👋"
        let unnamed = StickerPackComposer.Entry(payload: payload, preview: Self.picture)
        try await assertComposer(StickerPackComposer(
            name: "Penguins", summary: "For when words are too slow.", entries: [wave, unnamed]
        ))
    }

    func testNewPackThatFailedToUpload() async throws {
        let payload = AttachmentPayload(
            data: Self.pictureData, filename: "sticker-1.png", mediaType: "image/png", width: 256, height: 256
        )
        try await assertComposer(StickerPackComposer(
            name: "Penguins",
            entries: [StickerPackComposer.Entry(payload: payload, preview: Self.picture)],
            errorMessage: "Couldn't start the upload. Check your connection."
        ))
    }

    private func assertComposer(_ composer: StickerPackComposer, testName: String = #function) async throws {
        let sample = SampleSession()
        try await assertScreenSnapshot(
            of: sample.host(StickerPackComposerSheet(composer: composer) { _ in }),
            size: Self.sheet,
            asSheet: true,
            testName: testName
        )
    }

    // MARK: - The pack behind a sticker

    private static let priyasPack = StickerPackLocation(owner: SampleSession.priya, packID: "penguins")

    private static func penguins(isRestricted: Bool = false) -> StickerPack {
        StickerPack(
            id: "penguins",
            name: "Penguins",
            summary: "For when words are too slow.",
            isRestricted: isRestricted,
            stickers: ["👋", "🎉", "😴", "🐟", "❄️"].map(sticker)
        )
    }

    func testSomeoneElsesPack() async throws {
        let sample = SampleSession()
        sample.port.stickers.packsElsewhere[Self.priyasPack] = .success(Self.penguins())
        try await assertPack(sample)
    }

    func testAPackAlreadyInYourStickers() async throws {
        let sample = SampleSession()
        sample.port.stickers.packsElsewhere[Self.priyasPack] = .success(Self.penguins())
        sample.port.stickers.packs = .success([Self.penguins()])
        try await assertPack(sample)
    }

    func testAPackItsAuthorKeepsToThemselves() async throws {
        let sample = SampleSession()
        sample.port.stickers.packsElsewhere[Self.priyasPack] = .success(Self.penguins(isRestricted: true))
        try await assertPack(sample)
    }

    func testAPackThatIsGone() async throws {
        try await assertPack(SampleSession())
    }

    private func assertPack(_ sample: SampleSession, testName: String = #function) async throws {
        let sheet = StickerPackSheet(location: Self.priyasPack, samplePicture: Self.picture)
        try await assertScreenSnapshot(of: sample.host(sheet), size: Self.sheet, asSheet: true, testName: testName)
    }

    // MARK: - A sticker that needs no network

    private static func sticker(_ description: String) -> Sticker {
        Sticker(
            description: description,
            mediaType: "image/png",
            sources: [URL(string: "https://upload.waddle.test/stickers/sample.png")!]
        )
    }

    /// A round two-tone sticker on a clear ground, drawn here so it never
    /// changes.
    private static let pictureData: Data = {
        let side = 256
        let context = CGContext(
            data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        context.setFillColor(red: 0.07, green: 0.33, blue: 0.48, alpha: 1)
        context.fillEllipse(in: CGRect(x: 16, y: 16, width: side - 32, height: side - 32))
        context.setFillColor(red: 0.86, green: 0.94, blue: 0.97, alpha: 1)
        context.fillEllipse(in: CGRect(x: 72, y: 40, width: side - 144, height: side - 112))
        let data = NSMutableData()
        let destination = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil)!
        CGImageDestinationAddImage(destination, context.makeImage()!, nil)
        CGImageDestinationFinalize(destination)
        return data as Data
    }()

    private static var picture: Image {
        #if os(macOS)
        Image(nsImage: NSImage(data: pictureData)!)
        #else
        Image(uiImage: UIImage(data: pictureData)!)
        #endif
    }
}
