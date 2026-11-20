import SwiftUI
import WaddleKit

/// The account's sticker packs: tap a sticker to send it, make a new
/// pack, or remove one.
struct StickerPickerSheet: View {
    @Environment(SessionCoordinator.self) private var session
    let onPick: (Sticker, StickerPack) -> Void
    let onCancel: () -> Void
    /// Snapshots draw this instead of fetching each sticker's picture.
    var samplePicture: Image?

    @State private var selectedPackID: String?
    @State private var isCreating = false
    @State private var packToRemove: StickerPack?
    @State private var errorMessage: String?

    var body: some View {
        NavigationStack {
            content
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Theme.Colors.bg)
                .navigationTitle("Stickers")
                #if os(iOS)
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("Close", systemImage: "xmark", action: onCancel)
                            .help("Close")
                    }
                    ToolbarItem(placement: .primaryAction) {
                        Button("New pack", systemImage: "plus") { isCreating = true }
                            .help("New sticker pack")
                    }
                }
        }
        .presentationDetents([.medium, .large])
        #if os(macOS)
        .frame(minWidth: 440, minHeight: 480)
        #endif
        .task { await session.loadStickerPacks() }
        .sheet(isPresented: $isCreating) {
            StickerPackComposerSheet { pack in selectedPackID = pack.id }
                .environment(session)
        }
        .confirmationDialog(
            "Remove \(packToRemove?.title ?? "this pack")?",
            isPresented: Binding(get: { packToRemove != nil }, set: { if !$0 { packToRemove = nil } }),
            titleVisibility: .visible,
            presenting: packToRemove
        ) { pack in
            Button("Remove pack", role: .destructive) { remove(pack) }
        } message: { _ in
            Text("Stickers you already sent stay in their conversations.")
        }
    }

    @ViewBuilder
    private var content: some View {
        let packs = session.stickers.packs
        if let pack = packs.first(where: { $0.id == selectedPackID }) ?? packs.first {
            StickerPackPage(
                packs: packs,
                pack: pack,
                errorMessage: errorMessage,
                samplePicture: samplePicture,
                onSelectPack: { selectedPackID = $0.id },
                onPick: onPick,
                onRemove: { packToRemove = $0 }
            )
        } else {
            switch session.stickers.phase {
            case .idle, .loading:
                ProgressView()
            case .failed:
                status(
                    EmptyStateView(
                        title: "Couldn't load your stickers",
                        message: "Check your connection and try again.",
                        symbol: "exclamationmark.triangle"
                    ),
                    action: "Try again"
                ) {
                    Task { await session.loadStickerPacks() }
                }
            case .loaded:
                status(
                    EmptyStateView(
                        title: "No stickers yet",
                        message: "Make a pack from your own pictures, then send them from here.",
                        symbol: "face.smiling"
                    ),
                    action: "New sticker pack"
                ) {
                    isCreating = true
                }
            }
        }
    }

    private func status(_ state: EmptyStateView, action: String, perform: @escaping () -> Void) -> some View {
        VStack(spacing: 0) {
            state.fixedSize(horizontal: false, vertical: true)
            Button(action, action: perform)
                .buttonStyle(.huddle(.primary))
        }
    }

    private func remove(_ pack: StickerPack) {
        errorMessage = nil
        Task {
            do {
                try await session.removeStickerPack(pack.id)
            } catch {
                errorMessage = ActionErrorCopy.message(for: error, fallback: "Couldn't remove \(pack.title). Try again.")
            }
        }
    }
}

/// One pack's stickers, under the row that switches between packs.
struct StickerPackPage: View {
    let packs: [StickerPack]
    let pack: StickerPack
    let errorMessage: String?
    let samplePicture: Image?
    let onSelectPack: (StickerPack) -> Void
    let onPick: (Sticker, StickerPack) -> Void
    let onRemove: (StickerPack) -> Void

    private let columns = [GridItem(.adaptive(minimum: StickerTile.side), spacing: Theme.Spacing.xs)]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Theme.Spacing.m) {
                if packs.count > 1 {
                    packChips
                }
                header
                if let errorMessage {
                    Label(errorMessage, systemImage: "exclamationmark.triangle")
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.dangerText)
                }
                LazyVGrid(columns: columns, alignment: .leading, spacing: Theme.Spacing.xs) {
                    ForEach(Array(pack.stickers.enumerated()), id: \.offset) { _, sticker in
                        StickerCell(sticker: sticker, samplePicture: samplePicture) {
                            onPick(sticker, pack)
                        }
                    }
                }
            }
            .padding(Theme.Spacing.l)
        }
    }

    private var packChips: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Theme.Spacing.xs) {
                ForEach(packs) { candidate in
                    Button(candidate.title) { onSelectPack(candidate) }
                        .buttonStyle(.huddleChip(isSelected: candidate.id == pack.id))
                }
            }
        }
        // The selected chip's outline reaches past its frame.
        .scrollClipDisabled()
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline, spacing: Theme.Spacing.s) {
            VStack(alignment: .leading, spacing: Theme.Spacing.xxs) {
                Text(pack.title)
                    .huddleText(Theme.Typography.label)
                    .foregroundStyle(Theme.Colors.fg)
                    .accessibilityAddTraits(.isHeader)
                if let summary = pack.summary, !summary.isEmpty {
                    Text(summary)
                        .huddleText(Theme.Typography.caption)
                        .foregroundStyle(Theme.Colors.fgMuted)
                }
            }
            Spacer(minLength: Theme.Spacing.s)
            Menu {
                Button("Remove pack…", systemImage: "trash", role: .destructive) { onRemove(pack) }
            } label: {
                Image(systemName: "ellipsis")
                    .foregroundStyle(Theme.Colors.fgMuted)
                    .huddleTouchTarget()
            }
            .menuStyle(.button)
            .buttonStyle(.plain)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Pack options")
            .accessibilityLabel(Text("Options for \(pack.title)"))
        }
    }
}

/// One sticker in the picker; tap sends it.
struct StickerCell: View {
    let sticker: Sticker
    let samplePicture: Image?
    let onPick: () -> Void

    var body: some View {
        Button(action: onPick) {
            StickerTile(sticker: sticker, samplePicture: samplePicture)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(Text(sticker.text))
        .accessibilityAddTraits(.isImage)
        .accessibilityHint(Text("Sends this sticker"))
        .help(sticker.text)
    }
}

/// A sticker's picture on a small card, as the picker and a pack's page
/// lay stickers out.
struct StickerTile: View {
    let sticker: Sticker
    let samplePicture: Image?

    static let side: CGFloat = 76

    var body: some View {
        picture
            .padding(Theme.Spacing.xs)
            .frame(maxWidth: .infinity)
            .frame(height: Self.side)
            .huddleSurface(.card, radius: Theme.Radius.medium)
    }

    @ViewBuilder
    private var picture: some View {
        if let samplePicture {
            samplePicture.resizable().scaledToFit()
        } else if let url = sticker.sources.first {
            RemoteImageDataView(url: url) { phase in
                switch phase {
                case let .success(data):
                    AnimatedImageView(data: data, maxPixelSize: 240)
                case .failure:
                    Image(systemName: "photo.badge.exclamationmark")
                        .foregroundStyle(Theme.Colors.fgSoft)
                case .empty:
                    Color.clear
                }
            }
        }
    }
}
