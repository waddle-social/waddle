import AVFoundation
import WaddleKit

/// The system's microphone mode: read and watched here, changed only by the person in the system's picker.
enum SystemMicrophoneMode {
    static var active: CallMicrophoneMode {
        switch AVCaptureDevice.activeMicrophoneMode {
        case .standard: .standard
        case .voiceIsolation: .voiceIsolation
        case .wideSpectrum: .wideSpectrum
        @unknown default: .unknown
        }
    }

    @MainActor static func showPicker() {
        AVCaptureDevice.showSystemUserInterface(.microphoneModes)
    }

    @MainActor static func observe(_ update: (CallMicrophoneMode) -> Void) async {
        let (changes, continuation) = AsyncStream<Void>.makeStream(bufferingPolicy: .bufferingNewest(1))
        let observer = Observer(continuation: continuation)
        let key = #keyPath(AVCaptureDevice.activeMicrophoneMode)
        // The active mode belongs to the class, so observe the Objective-C class object.
        let device: AnyObject = AVCaptureDevice.self
        device.addObserver(observer, forKeyPath: key, options: .initial, context: nil)
        defer { device.removeObserver(observer, forKeyPath: key) }
        for await _ in changes {
            guard !Task.isCancelled else { break }
            update(active)
        }
    }

    private final class Observer: NSObject {
        let continuation: AsyncStream<Void>.Continuation

        init(continuation: AsyncStream<Void>.Continuation) {
            self.continuation = continuation
        }

        override func observeValue(
            forKeyPath keyPath: String?, of object: Any?, change: [NSKeyValueChangeKey: Any]?, context: UnsafeMutableRawPointer?
        ) {
            continuation.yield(())
        }
    }
}
