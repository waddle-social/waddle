import Foundation
import WaddleKit

extension FFIInbound {
    static func chatState(_ state: WaddleChatState) -> ChatState {
        switch state {
        case .active: return .active
        case .composing: return .composing
        case .paused: return .paused
        case .inactive: return .inactive
        case .gone: return .gone
        }
    }

    static func forumPostKind(_ kind: WaddleForumPostKind) -> ForumPostKind {
        switch kind {
        case .topic: return .topic
        case .reply: return .reply
        }
    }

    /// XEP-0394 span; a link span without an absolute URI is dropped.
    static func markupSpan(_ span: WaddleMarkupSpan) -> MarkupSpan? {
        guard span.end >= span.start, let kind = markupKind(span) else { return nil }
        return MarkupSpan(kind: kind, start: Int(span.start), end: Int(span.end))
    }

    static func markupKind(_ span: WaddleMarkupSpan) -> MarkupSpan.Kind? {
        switch span.spanType {
        case .bold: return .bold
        case .italic: return .italic
        case .strikethrough: return .strikethrough
        case .code: return .code
        case .codeBlock: return .codeBlock
        case .blockquote: return .blockquote
        case .link: return url(span.uri).map(MarkupSpan.Kind.link)
        }
    }

    static func reference(_ reference: WaddleReference) -> Reference {
        Reference(
            kind: referenceKind(reference.refType),
            uri: reference.uri,
            begin: Int(reference.begin),
            end: Int(reference.end)
        )
    }

    static func referenceKind(_ type: WaddleReferenceType) -> Reference.Kind {
        switch type {
        case .mention: return .mention
        case .data: return .data
        case let .other(value): return .other(value)
        }
    }

    /// XEP-0446/0447 file. Dropped when its URL does not parse, or when it
    /// is XEP-0448 ciphertext without a usable envelope: rendering
    /// ciphertext as the file would be wrong.
    static func sharedFile(_ file: WaddleSharedFile) -> SharedFile? {
        guard let location = url(file.url) else { return nil }
        var encrypted: EncryptedFileSource?
        if let envelope = file.encrypted {
            guard let source = encryptedSource(envelope) else { return nil }
            encrypted = source
        }
        return SharedFile(
            url: location,
            name: file.name,
            mediaType: file.mediaType,
            size: file.size.flatMap { Int(exactly: $0) },
            width: file.width.map(Int.init),
            height: file.height.map(Int.init),
            description: file.desc,
            disposition: file.disposition == "inline" ? .inline : .attachment,
            encrypted: encrypted
        )
    }

    /// XEP-0448 envelope; nil when none of its sources parse. The first
    /// digest per algorithm wins.
    static func encryptedSource(_ envelope: WaddleEncryptedFile) -> EncryptedFileSource? {
        let sources = envelope.sources.compactMap(url)
        guard !sources.isEmpty else { return nil }
        let hashes = Dictionary(envelope.hashes.map { ($0.algo, $0.valueB64) }, uniquingKeysWith: { first, _ in first })
        return EncryptedFileSource(
            cipher: envelope.cipher,
            keyBase64: envelope.keyB64,
            ivBase64: envelope.ivB64,
            hashes: hashes,
            sources: sources
        )
    }

    /// `urn:waddle:link-preview:0` card, keyed on the normalized URL when
    /// the server supplied one.
    static func linkPreview(_ preview: WaddleLinkPreview) -> LinkPreview? {
        guard let location = url(preview.normalizedUrl) ?? url(preview.originalUrl) else { return nil }
        return LinkPreview(
            url: location,
            title: preview.title,
            summary: preview.description,
            imageURL: url(preview.image?.url),
            imageAlt: preview.image?.alt
        )
    }

    static func pinPreview(_ preview: WaddlePinPreview) -> PinPreview {
        PinPreview(
            author: jid(preview.authorJid),
            authorNick: preview.authorNick,
            text: preview.text,
            messageTimestamp: date(preview.messageTimestamp)
        )
    }

    static func pinEvent(_ event: WaddlePinEvent) -> PinEvent {
        PinEvent(
            action: event.action == .pinned ? .pinned : .unpinned,
            targetStanzaID: event.targetStanzaId,
            by: jid(event.by),
            preview: event.preview.map(pinPreview)
        )
    }

    static func pinEntry(_ entry: WaddlePinEntry) -> PinEntry {
        PinEntry(
            targetStanzaID: entry.targetStanzaId,
            pinner: jid(entry.pinnerJid),
            pinnedAt: date(entry.pinnedAt),
            preview: pinPreview(entry.preview)
        )
    }

    /// XEP-0490 cursor. MUC private-message cursors (a full occupant JID
    /// as the item id) have no WaddleKit conversation and are dropped
    /// rather than folded onto the room.
    static func displayedCursor(_ entry: WaddleMdsDisplayedEntry) -> DisplayedCursor? {
        guard let conversation = bareJID(entry.chatId), let authority = bareJID(entry.stanzaIdBy) else { return nil }
        return DisplayedCursor(conversation: conversation, stanzaID: entry.stanzaId, stanzaIDBy: authority)
    }
}
