import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the Feed and stories: the Feed filled,
/// empty and after a failed refresh, a story open in the viewer, and the
/// story composer before and after a picture is chosen. These screens
/// have no design frame; they are drawn from the styleguide's components.
/// Text fields and scroll views only draw in a real window, so these are
/// screen snapshots, compared on the OS the goldens were recorded on (see
/// `assertScreenSnapshot`).
///
/// After an intended visual change, delete the affected goldens under
/// `__Snapshots__/CommunitySnapshotTests` and run the tests once to
/// record new ones (that run fails), then commit them.
@MainActor
final class CommunitySnapshotTests: XCTestCase {
    #if os(iOS)
    private static let screen = CGSize.phone
    private static let viewer = CGSize.phone
    private static let sheet = CGSize.phone
    #else
    private static let screen = CGSize(width: 760, height: 720)
    private static let viewer = CGSize(width: 420, height: 720)
    private static let sheet = CGSize(width: 440, height: 600)
    #endif

    // MARK: - Feed

    func testFeed() async throws {
        let sample = SampleSession()
        sample.seedCommunity()
        try await assertFeed(sample)
    }

    func testFeedWithNothingPosted() async throws {
        let sample = SampleSession()
        let store = sample.coordinator.community
        _ = store.begin()
        store.finish(feedFailed: false, storiesFailed: false)
        try await assertFeed(sample)
    }

    func testFeedThatDidNotLoad() async throws {
        let sample = SampleSession()
        let store = sample.coordinator.community
        _ = store.begin()
        store.finish(feedFailed: true, storiesFailed: true)
        try await assertFeed(sample)
    }

    private func assertFeed(_ sample: SampleSession, testName: String = #function) async throws {
        let feed = NavigationStack { FeedView(clock: date(0)) }
        try await assertScreenSnapshot(of: sample.host(feed), size: Self.screen, testName: testName)
    }

    // MARK: - Story viewer

    func testStoryWithReactions() async throws {
        let sample = SampleSession()
        sample.seedCommunity()
        let viewer = StoryViewer(
            start: SampleSession.rink,
            stories: sample.coordinator.community.activeStories(at: date(0)),
            now: date(0),
            picture: Self.picture
        )
        try await assertScreenSnapshot(of: sample.host(viewer), size: Self.viewer)
    }

    // MARK: - Story composer

    func testNewStoryWithoutAPicture() async throws {
        try await assertComposer(StoryComposer())
    }

    func testNewStoryWithAPictureAndCaption() async throws {
        let picture = AttachmentPayload(
            data: Self.pictureData, filename: "Photo-1.png", mediaType: "image/png", width: 360, height: 480
        )
        try await assertComposer(StoryComposer(picture: picture, caption: "First ice of the season at the rink."))
    }

    func testNewStoryThatFailedToPost() async throws {
        try await assertComposer(StoryComposer(errorMessage: "Pictures in a story can be up to 10 MB."))
    }

    private func assertComposer(_ composer: StoryComposer, testName: String = #function) async throws {
        let sample = SampleSession()
        try await assertScreenSnapshot(
            of: sample.host(StoryComposerSheet(composer: composer)), size: Self.sheet, asSheet: true, testName: testName
        )
    }

    // MARK: - A picture that needs no network

    /// A flat two-tone picture, drawn here so it never changes.
    private static let pictureData: Data = {
        let width = 360, height = 480
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpace(name: CGColorSpace.sRGB)!, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )!
        context.setFillColor(red: 0.07, green: 0.33, blue: 0.48, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height))
        context.setFillColor(red: 0.86, green: 0.94, blue: 0.97, alpha: 1)
        context.fill(CGRect(x: 0, y: 0, width: width, height: height / 3))
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
