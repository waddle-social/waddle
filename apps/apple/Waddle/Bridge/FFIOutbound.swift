import Foundation
import WaddleKit

/// Pure WaddleKit → FFI conversions. Offsets that do not fit the FFI's
/// `u32` are dropped rather than wrapped.
enum FFIOutbound {
    /// `stanzaID` is stamped as the stanza `@id` and XEP-0359
    /// `<origin-id/>`; for a send it must be the client id the local
    /// echo is keyed by.
    static func sendOptions(_ options: OutboundOptions, stanzaID: String?) -> WaddleSendOptions {
        WaddleSendOptions(
            stanzaId: stanzaID,
            subject: nil,
            reply: options.reply.map(replyTarget),
            fallback: options.reply?.fallback.flatMap(fallbackRange),
            thread: options.thread.map { WaddleThreadTarget(id: $0, parent: nil) },
            markupSpans: options.markupSpans.compactMap(markupSpan),
            references: options.references.compactMap(reference),
            sharedFiles: options.sharedFiles.map(sharedFile),
            linkPreviewToken: nil,
            requestDisplayedMarker: options.requestDisplayedMarker,
            mucPm: false,
            sticker: nil
        )
    }

    static func replyTarget(_ reply: OutboundOptions.Reply) -> WaddleReplyTarget {
        WaddleReplyTarget(authorJid: reply.author.description, messageId: reply.targetID)
    }

    static func fallbackRange(_ range: Range<Int>) -> WaddleFallbackRange? {
        guard let start = UInt32(exactly: range.lowerBound), let end = UInt32(exactly: range.upperBound) else { return nil }
        return WaddleFallbackRange(start: start, end: end)
    }

    static func markupSpan(_ span: MarkupSpan) -> WaddleMarkupSpan? {
        guard let start = UInt32(exactly: span.start), let end = UInt32(exactly: span.end) else { return nil }
        let (type, uri) = markupType(span.kind)
        return WaddleMarkupSpan(spanType: type, start: start, end: end, uri: uri)
    }

    static func markupType(_ kind: MarkupSpan.Kind) -> (WaddleMarkupSpanType, String?) {
        switch kind {
        case .bold: return (.bold, nil)
        case .italic: return (.italic, nil)
        case .strikethrough: return (.strikethrough, nil)
        case .code: return (.code, nil)
        case .codeBlock: return (.codeBlock, nil)
        case .blockquote: return (.blockquote, nil)
        case let .link(url): return (.link, url.absoluteString)
        }
    }

    static func reference(_ reference: Reference) -> WaddleReference? {
        guard let begin = UInt32(exactly: reference.begin), let end = UInt32(exactly: reference.end) else { return nil }
        return WaddleReference(refType: referenceType(reference.kind), uri: reference.uri, begin: begin, end: end, anchor: nil)
    }

    static func referenceType(_ kind: Reference.Kind) -> WaddleReferenceType {
        switch kind {
        case .mention: return .mention
        case .data: return .data
        case let .other(value): return .other(value: value)
        }
    }

    static func sharedFile(_ file: SharedFile) -> WaddleSharedFile {
        WaddleSharedFile(
            url: file.url.absoluteString,
            name: file.name,
            mediaType: file.mediaType,
            size: file.size.flatMap { UInt64(exactly: $0) },
            width: file.width.flatMap { UInt32(exactly: $0) },
            height: file.height.flatMap { UInt32(exactly: $0) },
            desc: file.description,
            hashes: [],
            disposition: disposition(forMediaType: file.mediaType),
            encrypted: file.encrypted.map(encryptedFile)
        )
    }

    /// XEP-0446 disposition inferred from the media type: media a client
    /// renders in place is `inline`, everything else an `attachment`.
    static func disposition(forMediaType mediaType: String?) -> String {
        guard let mediaType else { return "attachment" }
        let rendersInline = mediaType.hasPrefix("image/")
            || mediaType.hasPrefix("video/")
            || mediaType.hasPrefix("audio/")
            || mediaType == "application/pdf"
        return rendersInline ? "inline" : "attachment"
    }

    static func encryptedFile(_ source: EncryptedFileSource) -> WaddleEncryptedFile {
        WaddleEncryptedFile(
            cipher: source.cipher.rawValue,
            keyB64: source.keyBase64,
            ivB64: source.ivBase64,
            hashes: source.digests.xep0300.map { WaddleEncryptedFileHash(algo: $0.algorithm, valueB64: $0.base64) },
            sources: source.sources.map(\.absoluteString)
        )
    }
}
