import SwiftUI
import WaddleKit

/// Quick reactions on top, then the full emoji grid.
struct MessageEmojiPicker: View {
    let onPick: (String) -> Void

    private let columns = [GridItem(.adaptive(minimum: 38), spacing: Theme.Spacing.xs)]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                HStack(spacing: Theme.Spacing.xs) {
                    ForEach(Theme.quickReactions, id: \.self) { emoji in
                        emojiButton(emoji)
                    }
                }
                ForEach(EmojiCatalog.groups) { group in
                    Text(group.title)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .accessibilityAddTraits(.isHeader)
                    LazyVGrid(columns: columns, spacing: Theme.Spacing.xs) {
                        ForEach(group.emoji, id: \.self) { emoji in
                            emojiButton(emoji)
                        }
                    }
                }
            }
            .padding(Theme.Spacing.m)
        }
        .frame(idealWidth: 320, maxWidth: 420, idealHeight: 360, maxHeight: 480)
    }

    private func emojiButton(_ emoji: String) -> some View {
        Button {
            onPick(emoji)
        } label: {
            Text(emoji)
                .font(.title2)
                .frame(width: 38, height: 38)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// The full picker as a sheet, opened from a row's context menu.
struct MessageReactionPickerSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    let item: TimelineItem
    let actions: MessageActionModel

    var body: some View {
        NavigationStack {
            MessageEmojiPicker { emoji in
                actions.react(emoji, to: item, session: session)
                dismiss()
            }
            .navigationTitle("Add reaction")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
        #if os(macOS)
        .frame(minWidth: 360, minHeight: 420)
        #endif
    }
}
