/// The system microphone mode shown in call settings.
public enum CallMicrophoneMode: Sendable {
    case standard, voiceIsolation, wideSpectrum, unknown

    public var title: String {
        switch self {
        case .standard: "Standard"
        case .voiceIsolation: "Voice Isolation"
        case .wideSpectrum: "Wide Spectrum"
        case .unknown: "Unknown mode"
        }
    }
}
