import Testing

@testable import WaddleKit

@Suite("Call microphone modes") struct CallMicrophoneModeTests {
    @Test(arguments: [
        (CallMicrophoneMode.standard, "Standard"),
        (.voiceIsolation, "Voice Isolation"),
        (.wideSpectrum, "Wide Spectrum"),
        (.unknown, "Unknown mode"),
    ])
    func aModeShowsItsName(mode: CallMicrophoneMode, title: String) {
        #expect(mode.title == title)
    }
}
