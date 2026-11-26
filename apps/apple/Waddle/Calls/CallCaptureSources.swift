import AVFoundation
import LiveKit
import WaddleKit

/// Where a call's media gets what only the hardware and the system's
/// consent can give: leave to capture, a camera, and a screen to share.
/// Everything else the controller does is with the room.
///
/// The app's adapter is `SystemCaptureSources`. Tests bring one that
/// answers at once and whose tracks take their frames from a buffer.
@MainActor
protocol CallCaptureSources {
    /// Asks the system, prompting the person the first time.
    func requestAccess(for kind: AVMediaType) async -> Bool
    /// A camera that is not yet published or capturing, with `processor` on it from before its first frame.
    func makeCamera(options: CameraCaptureOptions?, processor: any VideoProcessor) async -> LocalVideoTrack

    #if os(macOS)
    var isScreenCaptureAllowed: Bool { get }
    /// Shows the system's prompt and returns at once.
    func requestScreenCapture()
    /// What the system will let us capture now, or `nil` when it will not say.
    func screenContent() async -> CallScreenContent?
    #endif
}

#if os(macOS)
/// One listing of what can be shared: what to offer, and how to make a
/// track of each. A screen is captured with the applications that existed
/// when it was listed, so a track is made from the listing it came from.
@MainActor
struct CallScreenContent {
    var candidates = CallScreenCandidates()
    /// By `CallScreenSource.id`.
    var makeTrack: [String: @MainActor (ScreenShareCaptureOptions) async -> LocalVideoTrack] = [:]
}
#endif

struct SystemCaptureSources: CallCaptureSources {
    nonisolated init() {}

    func requestAccess(for kind: AVMediaType) async -> Bool {
        await AVCaptureDevice.requestAccess(for: kind)
    }

    func makeCamera(options: CameraCaptureOptions?, processor: any VideoProcessor) async -> LocalVideoTrack {
        await LocalVideoTrack.createCameraTrack(options: options, processor: processor)
    }

    #if os(macOS)
    var isScreenCaptureAllowed: Bool { CGPreflightScreenCaptureAccess() }

    func requestScreenCapture() { CGRequestScreenCaptureAccess() }

    func screenContent() async -> CallScreenContent? {
        guard let displays = try? await MacOSScreenCapturer.displaySources(),
              let windows = try? await MacOSScreenCapturer.windowSources() else { return nil }
        let main = CGMainDisplayID()
        var content = CallScreenContent()
        for display in displays {
            content.makeTrack["screen:\(display.displayID)"] = { options in
                await LocalVideoTrack.createMacOSScreenShareTrack(source: display, options: options)
            }
            content.candidates.screens.append(.init(
                number: display.displayID, width: display.width, height: display.height, isMain: display.displayID == main
            ))
        }
        for window in windows {
            content.makeTrack["window:\(window.windowID)"] = { options in
                await LocalVideoTrack.createMacOSScreenShareTrack(source: window, options: options)
            }
            content.candidates.windows.append(.init(
                number: window.windowID, title: window.title, application: window.owningApplication?.applicationName,
                bundleID: window.owningApplication?.bundleIdentifier, layer: window.windowLayer,
                isOnScreen: window.isOnScreen, size: window.frame.size
            ))
        }
        return content
    }
    #endif
}
