import SwiftUI
import UniformTypeIdentifiers
#if os(macOS)
import AppKit
#elseif os(iOS)
import UIKit
#endif

struct ChatComposerView: View {
    @Binding var text: String
    var placeholder: String = "Write a message"
    var isSending: Bool = false
    var canSend: Bool = true
    var channelName: String? = nil
    var replyingToMessage: ChatTimelineMessage? = nil
    var onCancelReply: (() -> Void)? = nil
    var onFileSelected: ((_ data: Data, _ fileName: String, _ mediaType: String) -> Void)? = nil
    var onGifSelected: ((_ url: String) -> Void)? = nil
    var isUploadingFile: Bool = false
    var mentionSuggestions: [ChatRoomMember] = []
    var onMentionQueryChanged: ((String?) -> Void)? = nil
    var usesOperationalChrome: Bool = false
    var usesCompactConversationChrome: Bool = false
    var onSend: () -> Void
    @State private var showEmojiPicker = false
    @State private var showGifPicker = false
    @State private var showFileImporter = false

    private var hasSendableText: Bool {
        !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        VStack(spacing: 0) {
            mentionSuggestionList

            composerReplyPreview

            VStack(spacing: 0) {
                TextField(placeholder, text: $text, axis: .vertical)
                    .lineLimit(1...6)
                    .font(.body)
                    .foregroundStyle(WaddleTheme.textPrimary)
                    .padding(.horizontal, 14)
                    .padding(.top, 12)
                    .padding(.bottom, 8)
                    .onSubmit { if hasSendableText { onSend() } }
#if os(macOS)
                    .onPasteCommand(of: [.image]) { providers in
                        handlePastedImages(providers)
                    }
#endif

                HStack(spacing: 16) {
                    attachmentPickerButton
                    gifPickerButton
                    emojiPickerButton

                    Spacer()

                    Button(action: onSend) {
                        Image(systemName: "paperplane.fill")
                            .font(.system(size: 14, weight: .bold))
                            .foregroundStyle(hasSendableText ? Color.white : WaddleTheme.textMuted)
                            .frame(width: 34, height: 34)
                            .background(
                                RoundedRectangle(cornerRadius: 12, style: .continuous)
                                    .fill(hasSendableText ? WaddleTheme.accent : WaddleTheme.surfaceRaised)
                            )
                    }
                    .disabled(!canSend || isSending || !hasSendableText)
                }
                .padding(.horizontal, 16)
                .padding(.bottom, 12)
            }
            .background(
                WaddleTheme.composerBackground,
                in: RoundedRectangle(cornerRadius: composerCornerRadius, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: composerCornerRadius, style: .continuous)
                    .strokeBorder(WaddleTheme.divider, lineWidth: 1)
            }
            .padding(.horizontal, usesCompactConversationChrome ? 12 : 14)
            .padding(.vertical, usesCompactConversationChrome ? 8 : 12)
        }
        .onChange(of: text) { _, newValue in
            updateMentionQuery(newValue)
        }
        .fileImporter(
            isPresented: $showFileImporter,
            allowedContentTypes: [.item],
            allowsMultipleSelection: true
        ) { result in
            handleImportedChatAttachments(result, onFileSelected: onFileSelected)
        }
    }

    @ViewBuilder
    private var mentionSuggestionList: some View {
        let emojis = emojiSuggestions
        if !mentionSuggestions.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(mentionSuggestions.prefix(8)) { member in
                        Button {
                            insertMention(member.displayName)
                        } label: {
                            Text("@\(member.displayName)")
                                .font(.caption.weight(.medium))
                                .padding(.horizontal, 10)
                                .padding(.vertical, 6)
                                .background(WaddleTheme.channelSelected, in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
        } else if !emojis.isEmpty {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 6) {
                    ForEach(emojis, id: \.name) { item in
                        Button {
                            insertEmoji(item.emoji)
                        } label: {
                            HStack(spacing: 4) {
                                Text(item.emoji)
                                Text(":\(item.name):")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 6)
                            .background(WaddleTheme.surfaceRaised, in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 6)
            }
        }
    }

    private func updateMentionQuery(_ text: String) {
        guard let atIndex = text.lastIndex(of: "@") else {
            onMentionQueryChanged?(nil)
            return
        }
        let afterAt = text[text.index(after: atIndex)...]
        if afterAt.contains(" ") || afterAt.contains("\n") {
            onMentionQueryChanged?(nil)
            return
        }
        let query = String(afterAt)
        onMentionQueryChanged?(query)
    }

    private func insertMention(_ username: String) {
        guard let atIndex = text.lastIndex(of: "@") else { return }
        text = String(text[text.startIndex..<atIndex]) + "@\(username) "
        onMentionQueryChanged?(nil)
    }

    private var emojiSuggestions: [(name: String, emoji: String)] {
        guard let colonIndex = text.lastIndex(of: ":") else { return [] }
        let afterColon = text[text.index(after: colonIndex)...]
        if afterColon.contains(" ") || afterColon.contains("\n") || afterColon.contains(":") { return [] }
        let query = String(afterColon).lowercased()
        guard query.count >= 2 else { return [] }
        return Self.emojiShortcodes.filter { $0.name.contains(query) }.prefix(8).map { $0 }
    }

    private func insertEmoji(_ emoji: String) {
        guard let colonIndex = text.lastIndex(of: ":") else { return }
        text = String(text[text.startIndex..<colonIndex]) + emoji
    }

    private static let emojiShortcodes: [(name: String, emoji: String)] = [
        ("thumbsup", "👍"), ("thumbsdown", "👎"), ("heart", "❤️"), ("fire", "🔥"),
        ("smile", "😊"), ("laugh", "😂"), ("cry", "😢"), ("angry", "😤"),
        ("think", "🤔"), ("cool", "😎"), ("love", "😍"), ("wink", "😉"),
        ("clap", "👏"), ("pray", "🙏"), ("wave", "👋"), ("muscle", "💪"),
        ("rocket", "🚀"), ("star", "⭐"), ("check", "✅"), ("cross", "❌"),
        ("100", "💯"), ("eyes", "👀"), ("party", "🎉"), ("tada", "🎉"),
        ("sparkles", "✨"), ("warning", "⚠️"), ("bug", "🐛"), ("bulb", "💡"),
        ("pin", "📌"), ("link", "🔗"), ("lock", "🔒"), ("key", "🔑"),
        ("bell", "🔔"), ("memo", "📝"), ("gear", "⚙️"), ("hammer", "🔨"),
        ("package", "📦"), ("truck", "🚚"), ("calendar", "📅"), ("clock", "⏰"),
        ("sun", "☀️"), ("moon", "🌙"), ("rainbow", "🌈"), ("umbrella", "☂️"),
        ("coffee", "☕"), ("pizza", "🍕"), ("beer", "🍺"), ("cake", "🎂"),
        ("penguin", "🐧"), ("duck", "🦆"), ("dog", "🐶"), ("cat", "🐱"),
        ("skull", "💀"), ("ghost", "👻"), ("robot", "🤖"), ("alien", "👽"),
        ("confused", "😕"), ("shrug", "🤷"), ("facepalm", "🤦"), ("salute", "🫡"),
        ("ok", "👌"), ("point_up", "☝️"), ("point_down", "👇"), ("raised_hands", "🙌"),
    ]


    @ViewBuilder
    private var composerReplyPreview: some View {
        if let reply = replyingToMessage {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Replying to \(reply.senderDisplayName)")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(WaddleTheme.accent)
                    Text(reply.body)
                        .font(.caption)
                        .foregroundStyle(WaddleTheme.textSecondary)
                        .lineLimit(1)
                }
                Spacer()
                Button { onCancelReply?() } label: {
                    Image(systemName: "xmark")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(WaddleTheme.textMuted)
                }
                .buttonStyle(.plain)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            .background(
                WaddleTheme.channelSelected,
                in: RoundedRectangle(cornerRadius: 14, style: .continuous)
            )
            .padding(.horizontal, usesCompactConversationChrome ? 12 : 14)
            .padding(.top, usesCompactConversationChrome ? 8 : 10)
        }
    }

    private var attachmentPickerButton: some View {
        Group {
            if isUploadingFile {
                ProgressView()
                    .frame(width: 20, height: 20)
            } else {
                Button {
                    showFileImporter = true
                } label: {
                    composerAccessoryLabel(systemName: "paperclip")
                }
                .buttonStyle(.plain)
            }
        }
    }

    private func handlePastedImages(_ providers: [NSItemProvider]) {
        guard let onFileSelected else { return }
        for provider in providers {
            guard let imageType = provider.registeredTypeIdentifiers
                .compactMap({ UTType($0) })
                .first(where: { $0.conforms(to: .image) }) else {
#if os(macOS)
                if provider.canLoadObject(ofClass: NSImage.self) {
                    provider.loadObject(ofClass: NSImage.self) { object, _ in
                        guard let image = object as? NSImage,
                              let tiffData = image.tiffRepresentation,
                              let bitmap = NSBitmapImageRep(data: tiffData),
                              let pngData = bitmap.representation(using: .png, properties: [:]) else { return }
                        let fileName = "paste-\(Int(Date().timeIntervalSince1970)).png"
                        Task { @MainActor in
                            onFileSelected(pngData, fileName, "image/png")
                        }
                    }
                    return
                }
#elseif os(iOS)
                if provider.canLoadObject(ofClass: UIImage.self) {
                    provider.loadObject(ofClass: UIImage.self) { object, _ in
                        guard let image = object as? UIImage,
                              let pngData = image.pngData() else { return }
                        let fileName = "paste-\(Int(Date().timeIntervalSince1970)).png"
                        Task { @MainActor in
                            onFileSelected(pngData, fileName, "image/png")
                        }
                    }
                    return
                }
#endif
                continue
            }

            provider.loadDataRepresentation(forTypeIdentifier: imageType.identifier) { data, _ in
                if let data {
                    let ext = imageType.preferredFilenameExtension ?? "png"
                    let mime = imageType.preferredMIMEType ?? "image/png"
                    let fileName = "paste-\(Int(Date().timeIntervalSince1970)).\(ext)"
                    Task { @MainActor in
                        onFileSelected(data, fileName, mime)
                    }
                    return
                }

                provider.loadFileRepresentation(forTypeIdentifier: imageType.identifier) { url, _ in
                    guard let url, let fileData = try? Data(contentsOf: url) else { return }
                    let ext = imageType.preferredFilenameExtension ?? url.pathExtension
                    let mime = imageType.preferredMIMEType ?? "image/png"
                    let fileName = "paste-\(Int(Date().timeIntervalSince1970)).\(ext)"
                    Task { @MainActor in
                        onFileSelected(fileData, fileName, mime)
                    }
                }
            }
            return
        }
    }

    private var gifPickerButton: some View {
        Button {
            showGifPicker.toggle()
        } label: {
            composerAccessoryLabel(systemName: "play.rectangle")
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showGifPicker) {
            ChatGifPickerView { url in
                showGifPicker = false
                onGifSelected?(url)
            }
        }
    }

    private var emojiPickerButton: some View {
        Button {
            showEmojiPicker.toggle()
        } label: {
            composerAccessoryLabel(systemName: "face.smiling")
        }
        .buttonStyle(.plain)
        .popover(isPresented: $showEmojiPicker) {
            ChatEmojiPickerView { emoji in
                text += emoji
                showEmojiPicker = false
            }
        }
    }

    private var composerCornerRadius: CGFloat {
        usesCompactConversationChrome ? 18 : 20
    }

    @ViewBuilder
    private func composerAccessoryLabel(systemName: String) -> some View {
        Image(systemName: systemName)
            .font(.system(size: 14, weight: .semibold))
            .foregroundStyle(WaddleTheme.textSecondary)
            .frame(width: 28, height: 28)
    }

}
