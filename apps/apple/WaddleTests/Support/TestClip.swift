import AVFoundation
import XCTest

/// Short movies written with AVFoundation for tests, removed in
/// `tearDown`.
@MainActor
class ClipTestCase: XCTestCase {
    private var files: [URL] = []

    override func tearDown() async throws {
        for file in files { try? FileManager.default.removeItem(at: file) }
        try await super.tearDown()
    }

    func temporaryFile(_ fileExtension: String) -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("clip-test-\(UUID().uuidString)")
            .appendingPathExtension(fileExtension)
        files.append(url)
        return url
    }

    /// A second of grey frames in a QuickTime file, saying where it was
    /// filmed when `location` is given. Skips the test when this machine
    /// cannot encode `codec`.
    func clip(
        codec: AVVideoCodecType = .h264,
        width: Int = 64,
        height: Int = 64,
        filmedAt location: String? = nil
    ) async throws -> URL {
        let url = temporaryFile("mov")
        let writer = try AVAssetWriter(outputURL: url, fileType: .mov)
        if let location {
            let place = AVMutableMetadataItem()
            place.identifier = .quickTimeMetadataLocationISO6709
            place.dataType = kCMMetadataBaseDataType_UTF8 as String
            place.value = location as NSString
            writer.metadata = [place]
        }
        let settings: [String: Any] = [AVVideoCodecKey: codec, AVVideoWidthKey: width, AVVideoHeightKey: height]
        try XCTSkipUnless(writer.canApply(outputSettings: settings, forMediaType: .video), "No \(codec.rawValue) encoder here")
        let input = AVAssetWriterInput(mediaType: .video, outputSettings: settings)
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
            kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
            kCVPixelBufferWidthKey as String: width,
            kCVPixelBufferHeightKey as String: height,
        ])
        writer.add(input)
        XCTAssertTrue(writer.startWriting())
        writer.startSession(atSourceTime: .zero)
        for frame in 0..<10 {
            while !input.isReadyForMoreMediaData {
                try await Task.sleep(for: .milliseconds(5))
            }
            var buffer: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, try XCTUnwrap(adaptor.pixelBufferPool), &buffer)
            let pixels = try XCTUnwrap(buffer)
            CVPixelBufferLockBaseAddress(pixels, [])
            memset(CVPixelBufferGetBaseAddress(pixels), 0x60, CVPixelBufferGetDataSize(pixels))
            CVPixelBufferUnlockBaseAddress(pixels, [])
            adaptor.append(pixels, withPresentationTime: CMTime(value: CMTimeValue(frame), timescale: 10))
        }
        input.markAsFinished()
        await writer.finishWriting()
        try XCTSkipIf(
            codec != .h264 && writer.status != .completed,
            "Couldn't encode \(codec.rawValue) here: \(writer.error.map { "\($0)" } ?? "")"
        )
        XCTAssertEqual(writer.status, .completed, writer.error.map { "\($0)" } ?? "")
        return url
    }

    /// The codec of the first video track in the movie `data`.
    func videoCodec(of data: Data) async throws -> FourCharCode? {
        let file = temporaryFile("mp4")
        try data.write(to: file)
        let track = try await AVURLAsset(url: file).loadTracks(withMediaType: .video).first
        return try await track?.load(.formatDescriptions).first?.mediaSubType.rawValue
    }

    /// Location metadata in the movie `data`.
    func locations(in data: Data) async throws -> [AVMetadataItem] {
        let file = temporaryFile("mp4")
        try data.write(to: file)
        return try await AVURLAsset(url: file).load(.metadata).filter {
            $0.identifier == .quickTimeMetadataLocationISO6709 || $0.commonKey == .commonKeyLocation
        }
    }
}
