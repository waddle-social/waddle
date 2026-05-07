import SwiftUI

struct ChatNotificationToastView: View {
    let toast: ChatNotificationToast
    var onDismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "bell.badge.fill")
                .font(.body)
                .foregroundStyle(Color.accentColor)

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 4) {
                    Text(toast.senderName)
                        .font(.caption.weight(.semibold))
                    if let channel = toast.channelName {
                        Text("in #\(channel)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Text(toast.body)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
            }

            Spacer(minLength: 0)

            Button {
                onDismiss()
            } label: {
                Image(systemName: "xmark")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
        }
        .padding(12)
        .waddleGlass(in: .rect(cornerRadius: 14))
        .padding(.horizontal, 16)
        .transition(.move(edge: .top).combined(with: .opacity))
    }
}

struct ChatImageLightboxView: View {
    let file: XMPPSharedFile
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            AsyncImage(url: URL(string: file.url)) { phase in
                switch phase {
                case .success(let image):
                    image
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .ignoresSafeArea()
                case .failure:
                    VStack(spacing: 12) {
                        Image(systemName: "photo.badge.exclamationmark")
                            .font(.largeTitle)
                        Text("Failed to load image")
                            .font(.subheadline)
                    }
                    .foregroundStyle(.white.opacity(0.6))
                case .empty:
                    ProgressView()
                        .tint(.white)
                @unknown default:
                    EmptyView()
                }
            }
        }
        .overlay(alignment: .topTrailing) {
            Button {
                dismiss()
            } label: {
                Image(systemName: "xmark.circle.fill")
                    .font(.title2)
                    .foregroundStyle(.white.opacity(0.8))
                    .padding(16)
            }
        }
        .overlay(alignment: .bottom) {
            if let name = file.name, !name.isEmpty {
                Text(name)
                    .font(.caption)
                    .foregroundStyle(.white.opacity(0.7))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 6)
                    .background(.black.opacity(0.5), in: Capsule())
                    .padding(.bottom, 20)
            }
        }
#if os(iOS)
        .statusBarHidden()
#endif
    }
}

struct ChatEmojiPickerView: View {
    var onSelect: (String) -> Void
    @State private var searchText = ""

    private static let emojiCategories: [(name: String, emojis: [String])] = [
        ("Smileys", ["😀", "😂", "🥹", "😍", "🤩", "😎", "🤔", "😅", "😢", "😤", "🥺", "😱", "🤗", "🫡", "🤝", "🙏"]),
        ("Reactions", ["👍", "👎", "❤️", "🔥", "🎉", "✅", "❌", "💯", "👀", "🚀", "💪", "🙌", "👏", "🤷", "💀", "😭"]),
        ("Objects", ["💬", "📎", "📌", "🔗", "💡", "⚡", "🎯", "🏷️", "📝", "🔔", "⭐", "🌟", "💎", "🛠️", "🔒", "🔑"]),
        ("Nature", ["🌈", "☀️", "🌙", "⭐", "🌊", "🌸", "🍀", "🌻", "🐧", "🦆", "🐝", "🦋", "🐳", "🌴", "🍄", "🌵"]),
    ]

    private var filteredEmojis: [(name: String, emojis: [String])] {
        if searchText.isEmpty { return Self.emojiCategories }
        return Self.emojiCategories.compactMap { category in
            let filtered = category.emojis.filter { _ in true }
            return filtered.isEmpty ? nil : (category.name, filtered)
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            TextField("Search emoji", text: $searchText)
                .textFieldStyle(.roundedBorder)
                .padding(10)

            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    ForEach(filteredEmojis, id: \.name) { category in
                        VStack(alignment: .leading, spacing: 6) {
                            Text(category.name)
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(.secondary)
                                .padding(.horizontal, 4)

                            LazyVGrid(columns: Array(repeating: GridItem(.fixed(36), spacing: 4), count: 8), spacing: 4) {
                                ForEach(category.emojis, id: \.self) { emoji in
                                    Button {
                                        onSelect(emoji)
                                    } label: {
                                        Text(emoji)
                                            .font(.title2)
                                            .frame(width: 36, height: 36)
                                    }
                                    .buttonStyle(.plain)
                                }
                            }
                        }
                    }
                }
                .padding(10)
            }
        }
        .frame(width: 340, height: 320)
    }
}

struct ChatLoadingStateView: View {
    var title: String = "Loading conversation…"

    var body: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text(title)
                .foregroundStyle(WaddleTheme.textSecondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct ChatEmptyStateView: View {
    var title: String
    var message: String?
    var systemImage: String = "bubble.left.and.bubble.right"

    init(title: String, message: String? = nil, systemImage: String = "bubble.left.and.bubble.right") {
        self.title = title
        self.message = message
        self.systemImage = systemImage
    }

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: systemImage)
                .font(.title2)
                .foregroundStyle(WaddleTheme.textMuted)
            Text(title)
                .font(.headline)
                .foregroundStyle(WaddleTheme.textSecondary)
            if let message {
                Text(message)
                    .foregroundStyle(WaddleTheme.textMuted)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

struct ChatErrorStateView: View {
    var title: String = "Something went wrong"
    var message: String
    var retryTitle: String = "Try again"
    var onRetry: (() -> Void)? = nil

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.title2)
                .foregroundStyle(WaddleTheme.textMuted)
            Text(title)
                .font(.headline)
            Text(message)
                .foregroundStyle(WaddleTheme.textSecondary)
                .multilineTextAlignment(.center)
            if let onRetry {
                Button(retryTitle, action: onRetry)
                    .buttonStyle(.borderedProminent)
                    .padding(.top, 4)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(24)
    }
}

/// Sheet-presented panel showing a thread root plus its XEP-0201 children.
/// The panel composer posts replies that carry `<thread>root.id</thread>` so
/// the resulting messages cluster back into this view on arrival.
