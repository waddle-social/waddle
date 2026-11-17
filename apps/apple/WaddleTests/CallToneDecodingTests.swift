import AVFoundation
import WaddleKit
import XCTest

/// The synthesised tones are real audio files to the system's player. It
/// only decodes them here: CI machines have nothing to play through.
final class CallToneDecodingTests: XCTestCase {
    func testTheSystemPlayerReadsEachToneAtItsFullLength() throws {
        for tone in CallTone.allCases {
            let player = try AVAudioPlayer(data: tone.waveData(), fileTypeHint: AVFileType.wav.rawValue)
            XCTAssertEqual(player.duration, tone.duration, accuracy: 0.01, "\(tone)")
            XCTAssertEqual(player.numberOfChannels, 1, "\(tone)")
            XCTAssertEqual(player.format.sampleRate, Double(CallTone.sampleRate), "\(tone)")
        }
    }
}
