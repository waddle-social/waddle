import SwiftUI
import XCTest
@testable import Waddle
@testable import WaddleKit

/// Night and Daylight goldens for the Feed and stories: the Feed filled,
/// empty and after a failed refresh, the post composer with a link, a
/// picture and a paused video open in the viewer, and the story composer
/// before and after a picture or a video is chosen. These screens
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
    private static let composers = CGSize(width: 393, height: 360)
    #else
    private static let screen = CGSize(width: 760, height: 720)
    private static let viewer = CGSize(width: 420, height: 720)
    private static let sheet = CGSize(width: 440, height: 600)
    private static let composers = CGSize(width: 560, height: 320)
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

    /// One link that will be posted, and one that is not a web address.
    func testNewPostWithALink() async throws {
        let sample = SampleSession()
        let composers = VStack(spacing: Theme.Spacing.l) {
            FeedComposerCard(text: "The rink opens at seven tomorrow.", link: "waddle.social/rink")
            FeedComposerCard(text: "Skate times for the week", link: "the rink page")
            Spacer(minLength: 0)
        }
        .padding(Theme.Spacing.l)
        .background(Theme.Colors.bg)
        try await assertScreenSnapshot(of: sample.host(composers), size: Self.composers)
    }

    private func assertFeed(_ sample: SampleSession, testName: String = #function) async throws {
        let feed = NavigationStack { FeedView(clock: date(0)) }
        try await assertScreenSnapshot(of: sample.host(feed), size: Self.screen, testName: testName)
    }

    // MARK: - Story viewer

    func testStoryWithReactions() async throws {
        let sample = SampleSession()
        sample.seedCommunity()
        let stories = sample.coordinator.community.activeStories(at: date(0))
        try await assertViewer(StoryPlaythrough(start: SampleSession.rink, among: stories), in: sample)
    }

    /// The picture stands in for the video's frame.
    func testVideoStoryPausedAndMuted() async throws {
        let sample = SampleSession()
        sample.seedCommunity()
        let clip = Story(
            id: "story-clip",
            body: "Morning skate before the crowds.",
            media: URL(string: "https://upload.waddle.test/story-clip.mp4")!,
            mediaType: "video/mp4",
            author: SampleSession.jo,
            postedAt: date(-600),
            expiresAt: date(80_000)
        )
        var playthrough = StoryPlaythrough(start: clip, among: [SampleSession.rink, clip])
        playthrough.isMuted = true
        let video = StoryVideoPlayback(storyID: clip.id, isPaused: true, progress: 0.4)
        try await assertViewer(playthrough, video: video, in: sample)
    }

    private func assertViewer(
        _ playthrough: StoryPlaythrough,
        video: StoryVideoPlayback? = nil,
        in sample: SampleSession,
        testName: String = #function
    ) async throws {
        let viewer = StoryViewer(playthrough: playthrough, now: date(0), standIn: Self.picture, video: video)
        // A sheet on a Mac, the whole screen on iPhone.
        #if os(macOS)
        try await assertScreenSnapshot(
            of: sample.host(viewer.frame(width: 420, height: 720)), size: Self.viewer, asSheet: true, testName: testName
        )
        #else
        try await assertScreenSnapshot(of: sample.host(viewer), size: Self.viewer, testName: testName)
        #endif
    }

    // MARK: - Story composer

    func testNewStoryWithoutAPicture() async throws {
        try await assertComposer(StoryComposer())
    }

    func testNewStoryWithAPictureAndCaption() async throws {
        let picture = AttachmentPayload(
            data: Self.pictureData, filename: "Photo-1.png", mediaType: "image/png", width: 360, height: 480
        )
        try await assertComposer(StoryComposer(media: .picture(picture), caption: "First ice of the season at the rink."))
    }

    /// The picture stands in for the video's first frame.
    func testNewStoryWithAVideo() async throws {
        let clip = AttachmentPayload(data: Data(count: 64), filename: "story.mp4", mediaType: "video/mp4", width: nil, height: nil)
        try await assertComposer(StoryComposer(media: .video(clip), preview: Self.picture, caption: "Morning skate before the crowds."))
    }

    /// A video whose first frame could not be made.
    func testNewStoryWithAVideoWithoutAFrame() async throws {
        let clip = AttachmentPayload(data: Data(count: 64), filename: "story.mp4", mediaType: "video/mp4", width: nil, height: nil)
        try await assertComposer(StoryComposer(media: .video(clip)))
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
        let width = 360
        let height = 480
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
