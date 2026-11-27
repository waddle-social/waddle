import Foundation
import Testing
import WaddleKit

struct MessageAttachmentKindTests {
    @Test(arguments: [
        ("IMAGE/JPEG", "object", MessageAttachmentKind.image),
        ("VIDEO/MP4", "object", .video),
        ("Audio/MP4", "object", .audio),
        ("application/pdf", "object", .pdf),
        ("application/octet-stream", "photo.PNG?token=abc", .other),
        ("audio/mp4", "clip.MP4#t=1", .audio),
        ("application/octet-stream", "sound.OGA?token=abc", .other),
        ("application/octet-stream", "handout.PDF", .other),
        ("video/mp4", "cover.png", .video),
        ("application/pdf", "voice.wav", .pdf),
        ("application/zip", "cover.png", .archive),
        ("application/octet-stream", "bundle.zip", .other),
        ("text/plain", "notes.txt", .other),
    ])
    func declaredTypeWinsOverTheExtension(type: String, path: String, expected: MessageAttachmentKind) {
        let url = URL(string: "https://files.waddle.test/\(path)")!
        let file = SharedFile(url: url, mediaType: type, disposition: .inline)
        #expect(MessageAttachmentKind(file) == expected)
        #expect(MessageAttachmentKind.inlineKind(for: file) == ([.archive, .other].contains(expected) ? nil : expected))
    }

    @Test(arguments: ["png", "jpg", "jpeg", "gif", "webp", "heic"])
    func supportedImageExtensionsAreInferred(suffix: String) {
        let file = SharedFile(url: URL(string: "https://files.waddle.test/photo.\(suffix)")!, disposition: .inline)
        #expect(MessageAttachmentKind.inlineKind(for: file) == .image)
    }

    @Test(arguments: ["svg", "avif", "bmp"])
    func otherImageExtensionsUseFileCards(suffix: String) {
        let file = SharedFile(url: URL(string: "https://files.waddle.test/photo.\(suffix)")!, disposition: .inline)
        #expect(MessageAttachmentKind.inlineKind(for: file) == nil)
    }

    @Test(arguments: [
        ("photo.PNG", "opaque", MessageAttachmentKind.image),
        ("clip.MOV", "opaque", .video),
        ("voice.MP3", "opaque", .audio),
        ("report.PDF", "opaque", .pdf),
        ("archive.ZIP", "opaque", .archive),
        ("notes.TXT", "opaque", .other),
        ("report.pdf", "misleading.jpg", .pdf),
        ("download", "report.pdf", .pdf),
        ("download.bin", "report.pdf", .pdf),
    ])
    func recognizesFileNamesAndFallsBackToTheURL(name: String, path: String, expected: MessageAttachmentKind) {
        let file = SharedFile(url: URL(string: "https://files.waddle.test/\(path)")!, name: name)
        #expect(MessageAttachmentKind(file) == expected)
    }

    @Test func giphyIsAnImageEvenWithAnExtensionlessPath() {
        let file = SharedFile(url: URL(string: "https://media3.giphy.com/media/abcd")!, disposition: .inline)
        #expect(MessageAttachmentKind.inlineKind(for: file) == .image)
    }

    @Test func giphyMovieUsesTheVideoKindWithOrWithoutADeclaredType() {
        let url = URL(string: "https://media1.giphy.com/media/abcd/giphy.mp4")!
        for type in ["video/mp4", nil] {
            let file = SharedFile(url: url, mediaType: type, disposition: .inline)
            #expect(MessageAttachmentKind.inlineKind(for: file) == .video)
        }
    }

    @Test func explicitDownloadsStayFileCardsExceptDeclaredImages() {
        for name in ["clip.mp4", "sound.mp3", "report.pdf", "photo.png"] {
            let file = SharedFile(url: URL(string: "https://files.waddle.test/\(name)")!)
            #expect(MessageAttachmentKind.inlineKind(for: file) == nil)
        }
        let image = SharedFile(url: URL(string: "https://files.waddle.test/opaque")!, mediaType: "image/png")
        #expect(MessageAttachmentKind.inlineKind(for: image) == .image)
    }

    @Test(arguments: ["http://files.waddle.test/clip.mp4", "file:///clip.mp4", "https:clip.mp4"])
    func refusesNonHTTPSOrHostlessInlineSources(raw: String) {
        let file = SharedFile(url: URL(string: raw)!, disposition: .inline)
        #expect(file.inlineMediaURL == nil)
    }

    @Test func usesTheOriginalHTTPSURLIncludingItsQuery() {
        let url = URL(string: "https://files.waddle.test/clip.mp4?signed=abc")!
        #expect(SharedFile(url: url, disposition: .inline).inlineMediaURL == url)
        let encrypted = EncryptedFileSource(
            cipher: .aes256GCM, keyBase64: "", ivBase64: "", digests: FileDigests(), sources: [url]
        )
        #expect(SharedFile(url: url, disposition: .inline, encrypted: encrypted).inlineMediaURL == nil)
    }
}
