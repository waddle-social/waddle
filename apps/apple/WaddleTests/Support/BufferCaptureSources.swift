import AVFoundation
import CoreVideo
import LiveKit
import WaddleKit
@testable import Waddle

/// Capture with no hardware and nobody to ask: consent is whatever the
/// test says, and every camera and screen is a track fed from a buffer,
/// so it publishes to an SFU like a real one.
@MainActor
final class BufferCaptureSources: CallCaptureSources {
    var allows: [AVMediaType: Bool] = [.audio: true, .video: true]
    private(set) var asked: [AVMediaType] = []
    private(set) var cameras: [LocalVideoTrack] = []

    var isScreenCaptureAllowed = true
    private(set) var screenPrompts = 0
    /// The screens the system lists, by number; `nil` when it will not say.
    var screens: [UInt32]? = [1]
    private(set) var screenTracks: [LocalVideoTrack] = []
    /// How long a screen takes to start being captured, as a real one does.
    var screenStartDelay: Duration = .zero

    /// A camera made while this is off gets no frame, as one whose blur gives no picture.
    var feedsCameras = true
    /// The person has not answered the prompt yet; `answerPrompts` is their answer.
    var holdsAnswers = false
    private var waitingForAnAnswer: [CheckedContinuation<Void, Never>] = []

    private var feeds: [Task<Void, Never>] = []

    func requestAccess(for kind: AVMediaType) async -> Bool {
        asked.append(kind)
        if holdsAnswers { await withCheckedContinuation { waitingForAnAnswer.append($0) } }
        return allows[kind] ?? false
    }

    func answerPrompts() {
        holdsAnswers = false
        waitingForAnAnswer.forEach { $0.resume() }
        waitingForAnAnswer = []
    }

    func makeCamera(options: CameraCaptureOptions?, processor: any VideoProcessor) async -> LocalVideoTrack {
        let track = await fedTrack(source: .camera)
        track.capturer.processor = processor
        cameras.append(track)
        return track
    }

    #if os(macOS)
    func requestScreenCapture() { screenPrompts += 1 }

    func screenContent() async -> CallScreenContent? {
        guard let screens else { return nil }
        var content = CallScreenContent()
        for number in screens {
            content.candidates.screens.append(.init(number: number, width: 640, height: 360, isMain: number == screens.first))
            content.makeTrack["screen:\(number)"] = { [weak self] _ in
                if let delay = self?.screenStartDelay, delay > .zero { try? await Task.sleep(for: delay) }
                let track = await LocalVideoTrack.createBufferTrack(source: .screenShareVideo)
                self?.feed(track)
                self?.screenTracks.append(track)
                return track
            }
        }
        return content
    }
    #endif

    /// Ends the feeds; the tracks are the controller's to stop.
    func stop() {
        answerPrompts()
        feeds.forEach { $0.cancel() }
        feeds = []
    }

    private func fedTrack(source: Track.Source) async -> LocalVideoTrack {
        let track = await LocalVideoTrack.createBufferTrack(name: Track.cameraName, source: source)
        if feedsCameras { feed(track) }
        return track
    }

    /// A track is published once it has had a frame: ten a second until told to stop.
    private func feed(_ track: LocalVideoTrack) {
        feeds.append(Task {
            var buffer: CVPixelBuffer?
            CVPixelBufferCreate(nil, 640, 360, kCVPixelFormatType_32BGRA, nil, &buffer)
            while !Task.isCancelled, let buffer {
                (track.capturer as? BufferCapturer)?.capture(buffer)
                try? await Task.sleep(for: .milliseconds(100))
            }
        })
    }
}
