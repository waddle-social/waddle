import PhotosUI
import SwiftUI
import WaddleKit

/// Makes a sticker pack: name it, choose pictures, say what each shows.
struct StickerPackComposerSheet: View {
    @Environment(SessionCoordinator.self) private var session
    @Environment(\.dismiss) private var dismiss
    @State private var composer: StickerPackComposer
    @State private var pickedItems: [PhotosPickerItem] = []
    @State private var creating: Task<Void, Never>?
    let onCreated: (StickerPack) -> Void

    /// Snapshots hand in a composer already in the state they show.
    init(composer: StickerPackComposer? = nil, onCreated: @escaping (StickerPack) -> Void) {
        _composer = State(initialValue: composer ?? StickerPackComposer())
        self.onCreated = onCreated
    }

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Spacing.l) {
                    details
                    stickers
                    if let errorMessage = composer.errorMessage {
                        Label(errorMessage, systemImage: "exclamationmark.triangle")
                            .huddleText(Theme.Typography.caption)
                            .foregroundStyle(Theme.Colors.dangerText)
                    }
                    Text("Anyone you send a sticker to can see its picture. Pictures are made smaller, and details such as where they were taken are left out.")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
                .padding(Theme.Spacing.l)
            }
            .background(Theme.Colors.bg)
            .navigationTitle("New sticker pack")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") {
                        creating?.cancel()
                        dismiss()
                    }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(createTitle, action: create)
                        .disabled(!composer.canCreate)
                }
            }
            .interactiveDismissDisabled(composer.isCreating)
        }
        #if os(macOS)
        .frame(width: 440, height: 600)
        #endif
        .onChange(of: pickedItems) { _, items in
            guard !items.isEmpty else { return }
            pickedItems = []
            for item in items {
                Task {
                    await composer.add(mediaType: item.supportedContentTypes.first?.preferredMIMEType) {
                        try? await item.loadTransferable(type: Data.self)
                    }
                }
            }
        }
    }

    private var createTitle: String {
        if case let .creating(done, total) = composer.phase {
            return "Uploading \(min(done + 1, total)) of \(total)…"
        }
        return "Create"
    }

    private var details: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            field("Name", text: $composer.name, label: "Pack name")
            field("What it's about (optional)", text: $composer.summary, label: "What the pack is about")
        }
        .disabled(composer.isCreating)
    }

    private func field(_ prompt: String, text: Binding<String>, label: String) -> some View {
        TextField(prompt, text: text)
            .textFieldStyle(.plain)
            .huddleText(Theme.Typography.input)
            .foregroundStyle(Theme.Colors.fg)
            .padding(Theme.Spacing.m)
            .huddleSurface(.card, radius: Theme.Radius.medium)
            .environment(\.huddleGround, Theme.Colors.surface)
            .accessibilityLabel(Text(label))
    }

    private var stickers: some View {
        VStack(alignment: .leading, spacing: Theme.Spacing.s) {
            ForEach($composer.entries) { $entry in
                StickerPackEntryRow(entry: $entry) {
                    composer.remove(entry.id)
                }
            }
            .disabled(composer.isCreating)
            PhotosPicker(
                selection: $pickedItems,
                maxSelectionCount: max(1, StickerPackComposer.maxStickers - composer.entries.count),
                matching: .images
            ) {
                Label(composer.entries.isEmpty ? "Choose pictures" : "Add more pictures", systemImage: "photo.badge.plus")
            }
            .buttonStyle(.huddle(composer.entries.isEmpty ? .primary : .secondary))
            .disabled(composer.isCreating || composer.isFull)
        }
    }

    private func create() {
        creating = Task {
            if let pack = await composer.create(using: session) {
                onCreated(pack)
                dismiss()
            }
        }
    }
}

/// One picture of a new pack, with the words that describe it.
private struct StickerPackEntryRow: View {
    @Binding var entry: StickerPackComposer.Entry
    let onRemove: () -> Void

    var body: some View {
        HStack(spacing: Theme.Spacing.m) {
            picture
                .frame(width: 56, height: 56)
                .accessibilityHidden(true)
            TextField("Emoji or a few words", text: $entry.description)
                .textFieldStyle(.plain)
                .huddleText(Theme.Typography.input)
                .foregroundStyle(Theme.Colors.fg)
                .accessibilityLabel(Text("What this sticker shows"))
            Button(action: onRemove) {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(Theme.Colors.fgSoft)
                    .huddleTouchTarget()
            }
            .buttonStyle(.plain)
            .help("Remove this sticker")
            .accessibilityLabel(Text("Remove this sticker"))
        }
        .padding(Theme.Spacing.s)
        .huddleSurface(.card, radius: Theme.Radius.medium)
        .environment(\.huddleGround, Theme.Colors.surface)
    }

    @ViewBuilder
    private var picture: some View {
        if let preview = entry.preview {
            preview.resizable().scaledToFit()
        } else {
            Image(systemName: "photo")
                .foregroundStyle(Theme.Colors.fgSoft)
        }
    }
}
